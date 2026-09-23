//! Calls through a SIP registrar over TCP, as one of its users. Audio is
//! PCMA only, on the same RTP session as the wire, with no playout clock.

use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use bytesstr::BytesStr;
use ezk_sdp_types::{MediaType, SessionDescription, TaggedAddress};
use ezk_sip_auth::{DigestAuthenticator, DigestCredentials, DigestUser};
use ezk_sip_core::transport::TpHandle;
use ezk_sip_core::{Endpoint, IncomingRequest, Layer, MayTake};
use ezk_sip_types::header::typed::Contact;
use ezk_sip_types::uri::{NameAddr, SipUri, SipUriUserPart};
use ezk_sip_types::{Method, StatusCode};
use ezk_sip_ua::dialog::DialogLayer;
use ezk_sip_ua::invite::InviteLayer;
use ezk_sip_ua::{
    CallEvent, InboundCall, MakeCallCompletionError, MakeCallError, MediaBackend, NoMedia,
    RegistrarConfig, Registration,
};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::rtp::{Impairment, PCMA, Session, Signalling};
use crate::{Call, DialError, Incoming, Transport};

/// A user's account on a registrar.
#[derive(Clone)]
pub struct Account {
    /// Host name or address of the registrar, reached over TCP on 5060.
    pub registrar: String,
    pub user: String,
    pub password: String,
}

impl fmt::Debug for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Account")
            .field("registrar", &self.registrar)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

pub struct Sip {
    registration: Arc<Registration>,
    credentials: DigestCredentials,
    local_ip: IpAddr,
    invites: mpsc::UnboundedReceiver<Invite>,
    ringing: Option<(u64, InboundCall<NoMedia>)>,
    next_caller: u64,
    calls_up: Arc<AtomicUsize>,
    _transport: TpHandle,
}

type Invite = (String, InboundCall<NoMedia>);

impl fmt::Debug for Sip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sip")
            .field("local_ip", &self.local_ip)
            .finish_non_exhaustive()
    }
}

struct Invites {
    contact: Arc<OnceLock<Contact>>,
    calls: mpsc::UnboundedSender<Invite>,
    calls_up: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Layer for Invites {
    fn name(&self) -> &'static str {
        "softmodem-invites"
    }

    async fn receive(&self, endpoint: &Endpoint, request: MayTake<'_, IncomingRequest>) {
        if request.line.method == Method::OPTIONS {
            let mut options = request.take();
            let response = endpoint.create_response(&options, StatusCode::OK, None);
            let _ = endpoint
                .create_server_tsx(&mut options)
                .respond(response)
                .await;
            return;
        }
        if request.line.method != Method::INVITE {
            return;
        }
        let Some(contact) = self.contact.get().cloned() else {
            return;
        };
        let invite = request.take();
        let number = match &invite.base_headers.from.uri.uri.user_part {
            SipUriUserPart::User(user) => user.to_string(),
            _ => String::new(),
        };
        let call = match InboundCall::from_invite(endpoint.clone(), invite, contact) {
            Ok(call) => call,
            Err(error) => {
                warn!(error = %error.1, "unusable INVITE");
                return;
            }
        };
        if self.calls_up.load(Ordering::SeqCst) > 0 {
            info!(number, "busy, refusing a call");
            let _ = call.decline(StatusCode::BUSY_HERE, None).await;
            return;
        }
        let _ = self.calls.send((number, call));
    }
}

// Counts a call as up for as long as it lives.
struct CallUp(Arc<AtomicUsize>);

impl CallUp {
    fn new(calls_up: &Arc<AtomicUsize>) -> Self {
        calls_up.fetch_add(1, Ordering::SeqCst);
        Self(calls_up.clone())
    }
}

impl Drop for CallUp {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn other<E: fmt::Display>(error: E) -> io::Error {
    io::Error::other(error.to_string())
}

impl Sip {
    /// Registers `account` and keeps the registration alive while this lives.
    ///
    /// # Errors
    ///
    /// Fails if the registrar cannot be reached or refuses the account.
    pub async fn register(account: Account) -> io::Result<Self> {
        let contact = Arc::new(OnceLock::new());
        let calls_up = Arc::new(AtomicUsize::new(0));
        let (calls, invites) = mpsc::unbounded_channel();
        let mut builder = Endpoint::builder();
        builder.add_layer(DialogLayer::default());
        builder.add_layer(InviteLayer::default());
        builder.add_layer(Invites {
            contact: contact.clone(),
            calls,
            calls_up: calls_up.clone(),
        });
        builder.user_agent(format!("softmodem/{}", env!("CARGO_PKG_VERSION")));
        let endpoint = builder.build();

        let registrar: SipUri = format!("sip:{};transport=tcp", account.registrar)
            .parse()
            .map_err(other)?;
        let (transport, _) = endpoint.select_transport(&registrar).await.map_err(other)?;
        let bound = transport.bound();
        let ours: SipUri = format!("sip:{}@{bound};transport=tcp", account.user)
            .parse()
            .map_err(other)?;
        let _ = contact.set(Contact::new(NameAddr::uri(ours.clone())));

        let mut credentials = DigestCredentials::new();
        credentials.set_default(DigestUser::new(
            account.user.clone(),
            account.password.clone(),
        ));
        let config = RegistrarConfig::new(account.user.clone(), registrar)
            .with_override_contact(Contact::new(NameAddr::uri(ours)));
        let registration = Registration::register(
            endpoint.clone(),
            config,
            DigestAuthenticator::new(credentials.clone()),
        )
        .await
        .map_err(other)?;
        info!(user = account.user, registrar = account.registrar, %bound, "registered");

        Ok(Self {
            registration: Arc::new(registration),
            credentials,
            local_ip: bound.ip(),
            invites,
            ringing: None,
            next_caller: 0,
            calls_up,
            _transport: transport,
        })
    }

    fn authenticator(&self) -> DigestAuthenticator {
        DigestAuthenticator::new(self.credentials.clone())
    }
}

struct Dial {
    registration: Arc<Registration>,
    number: String,
    authenticator: DigestAuthenticator,
    media: Pcma,
    calls_up: Arc<AtomicUsize>,
}

impl Dial {
    // A task of its own, so a dial given up even before the first response is cancelled.
    async fn run(self, mut result: oneshot::Sender<Result<Call, DialError>>) {
        let mut outbound = match self
            .registration
            .make_call(self.number.clone(), self.authenticator, self.media)
            .await
        {
            Ok(outbound) => outbound,
            Err(MakeCallError::Failed(status)) => {
                let _ = result.send(Err(refused(status.code)));
                return;
            }
            Err(error) => {
                let _ = result.send(Err(DialError::Io(other(error))));
                return;
            }
        };
        let completion = tokio::select! {
            completion = outbound.wait_for_completion() => completion,
            () = result.closed() => {
                info!(number = self.number, "dial given up, cancelling");
                let _ = Box::pin(outbound.cancel()).await;
                return;
            }
        };
        let answered = match completion {
            Ok(unacknowledged) => unacknowledged.finish().await.map_err(other),
            Err(MakeCallCompletionError::Failed(status)) => {
                let _ = result.send(Err(refused(status.code)));
                return;
            }
            Err(error) => Err(other(error)),
        };
        let call = answered.and_then(|call| {
            info!(number = self.number, "answered");
            start(call, CallUp::new(&self.calls_up))
        });
        let _ = result.send(call.map_err(DialError::Io));
    }
}

impl Transport for Sip {
    type Caller = u64;

    async fn dial(&mut self, number: &str) -> Result<Call, DialError> {
        let dial = Dial {
            registration: self.registration.clone(),
            number: number.to_owned(),
            authenticator: self.authenticator(),
            media: Pcma::bind(self.local_ip).await?,
            calls_up: self.calls_up.clone(),
        };
        let (result, outcome) = oneshot::channel();
        tokio::spawn(Box::pin(dial.run(result)));
        outcome
            .await
            .map_err(|_| DialError::Io(other("the dial stopped")))?
    }

    async fn incoming(&mut self) -> io::Result<Incoming<u64>> {
        loop {
            let invite = match &mut self.ringing {
                Some((id, call)) => {
                    let id = *id;
                    tokio::select! {
                        () = call.cancelled() => {
                            self.ringing = None;
                            return Ok(Incoming::Gone(id));
                        }
                        invite = self.invites.recv() => invite,
                    }
                }
                None => self.invites.recv().await,
            };
            let (number, mut invite) = invite.ok_or_else(|| other("the SIP endpoint stopped"))?;
            if self.ringing.is_some() {
                let _ = invite.decline(StatusCode::BUSY_HERE, None).await;
                continue;
            }
            invite
                .respond_provisional(StatusCode::RINGING)
                .await
                .map_err(other)?;
            self.next_caller += 1;
            let id = self.next_caller;
            info!(number, "incoming call");
            self.ringing = Some((id, invite));
            return Ok(Incoming::Ringing { caller: id, number });
        }
    }

    async fn answer(&mut self, caller: &u64) -> io::Result<Call> {
        let Some((id, invite)) = self.ringing.take() else {
            return Err(other("no call is ringing"));
        };
        if id != *caller {
            self.ringing = Some((id, invite));
            return Err(other("that call is not ringing"));
        }
        let media = Pcma::bind(self.local_ip).await?;
        let call_up = CallUp::new(&self.calls_up);
        let call = invite.with_media(media).accept().await.map_err(other)?;
        start(call, call_up)
    }

    async fn reject(&mut self, caller: &u64) -> io::Result<()> {
        if let Some((id, invite)) = self.ringing.take() {
            if id == *caller {
                invite
                    .decline(StatusCode::BUSY_HERE, None)
                    .await
                    .map_err(other)?;
            } else {
                self.ringing = Some((id, invite));
            }
        }
        Ok(())
    }
}

fn refused(code: StatusCode) -> DialError {
    if matches!(code.into_u16(), 486 | 600 | 603) {
        DialError::Busy
    } else {
        DialError::Io(other(format!("call refused with {}", code.into_u16())))
    }
}

/// Joins a SIP call to an RTP session, each ending the other.
fn start(mut sip: ezk_sip_ua::Call<Pcma>, call_up: CallUp) -> io::Result<Call> {
    let media = sip.media();
    let peer = media
        .remote
        .ok_or_else(|| other("no remote media address"))?;
    let socket = media.socket.clone();
    let (stop, stopped) = oneshot::channel();
    let (ended, mut on_end) = oneshot::channel();
    let mut call = Session {
        socket,
        peer,
        signalling: Signalling::Separate,
        impairment: Impairment::default(),
    }
    .start(Some(stopped), Some(ended));

    let signalling = tokio::spawn(async move {
        let _call_up = call_up;
        let mut stop = Some(stop);
        loop {
            tokio::select! {
                event = sip.run() => match event {
                    Ok(CallEvent::Internal(event)) => {
                        if let Err(error) = sip.handle_internal_event(event).await {
                            warn!(%error, "SIP call failed");
                            break;
                        }
                    }
                    Ok(CallEvent::Media(())) => {}
                    Ok(CallEvent::Terminated) => break,
                    Err(error) => {
                        warn!(%error, "SIP call failed");
                        break;
                    }
                },
                _ = &mut on_end => {
                    let _ = sip.terminate().await;
                    return;
                }
            }
        }
        if let Some(stop) = stop.take() {
            let _ = stop.send(());
        }
    });
    call.tasks.push(signalling);
    Ok(call)
}

struct Pcma {
    socket: Arc<UdpSocket>,
    local: SocketAddr,
    remote: Option<SocketAddr>,
}

#[derive(Debug)]
struct NoPcma;

impl fmt::Display for NoPcma {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the far end offers no PCMA audio")
    }
}

impl std::error::Error for NoPcma {}

impl Pcma {
    async fn bind(ip: IpAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::new(ip, 0)).await?;
        let local = socket.local_addr()?;
        Ok(Self {
            socket: Arc::new(socket),
            local,
            remote: None,
        })
    }

    fn description(&self) -> SessionDescription {
        let family = if self.local.is_ipv4() { "IP4" } else { "IP6" };
        let ip = self.local.ip();
        let port = self.local.port();
        let text = format!(
            "v=0\r\n\
             o=softmodem {port} 1 IN {family} {ip}\r\n\
             s=softmodem\r\n\
             c=IN {family} {ip}\r\n\
             t=0 0\r\n\
             m=audio {port} RTP/AVP {PCMA}\r\n\
             a=rtpmap:{PCMA} PCMA/8000\r\n\
             a=ptime:20\r\n\
             a=sendrecv\r\n"
        );
        SessionDescription::parse(&BytesStr::from(text)).expect("the offer is well formed")
    }
}

fn remote_audio(sdp: &SessionDescription) -> Result<SocketAddr, NoPcma> {
    let media = sdp
        .media_descriptions
        .iter()
        .find(|m| {
            m.media.media_type == MediaType::Audio
                && m.media.port != 0
                && m.media.fmts.contains(&PCMA)
        })
        .ok_or(NoPcma)?;
    let connection = media
        .connection
        .as_ref()
        .or(sdp.connection.as_ref())
        .ok_or(NoPcma)?;
    let ip: IpAddr = match &connection.address {
        TaggedAddress::IP4(ip) => (*ip).into(),
        TaggedAddress::IP6(ip) => (*ip).into(),
        TaggedAddress::IP4FQDN(_) | TaggedAddress::IP6FQDN(_) => return Err(NoPcma),
    };
    Ok(SocketAddr::new(ip, media.media.port))
}

impl MediaBackend for Pcma {
    type Error = NoPcma;
    type Event = ();

    fn has_media(&self) -> bool {
        true
    }

    fn create_sdp_offer(
        &mut self,
    ) -> impl Future<Output = Result<SessionDescription, NoPcma>> + Send {
        std::future::ready(Ok(self.description()))
    }

    fn receive_sdp_answer(
        &mut self,
        sdp: SessionDescription,
    ) -> impl Future<Output = Result<(), NoPcma>> + Send {
        let remote = remote_audio(&sdp);
        self.remote = remote.as_ref().ok().copied();
        std::future::ready(remote.map(|_| ()))
    }

    fn receive_sdp_offer(
        &mut self,
        sdp: SessionDescription,
    ) -> impl Future<Output = Result<SessionDescription, NoPcma>> + Send {
        let remote = remote_audio(&sdp);
        self.remote = remote.as_ref().ok().copied();
        std::future::ready(remote.map(|_| self.description()))
    }

    async fn run(&mut self) -> Result<(), NoPcma> {
        std::future::pending().await
    }
}
