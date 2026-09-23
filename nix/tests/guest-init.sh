#!/bin/sh
# Runs as /init in a QEMU guest whose 16550 is the softmodem's CUSE port.
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev

status() {
    echo "guest $1: $(grep 16550A /proc/tty/driver/serial)"
}

port=/dev/ttyS$(grep 16550A /proc/tty/driver/serial | cut -d : -f 1)
stty -F "$port" 115200 raw -echo clocal
exec 3<>"$port"

status "before dialling"
printf 'ATE0\r' >&3
sleep 1
printf 'ATDT0300\r' >&3
if timeout 60 grep -q -m 1 CONNECT <&3; then
    echo "guest: CONNECT"
fi
sleep 1
status "connected"

sleep 1.2
printf '+++' >&3
sleep 1.2
printf 'ATH\r' >&3
sleep 2
status "hung up"

poweroff -f
