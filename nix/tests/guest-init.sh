#!/bin/sh
# Runs as /init in a QEMU guest whose 16550 is the softmodem's CUSE port.
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev

for module in /modules/*.ko; do
    [ -e "$module" ] && insmod "$module"
done

for device in /sys/class/tty/ttyS*; do
    case $(readlink "$device") in
    */pci0000:*) tty=${device##*/} ;;
    esac
done
port=/dev/$tty

status() {
    echo "guest $1: $(grep "^${tty#ttyS}:" /proc/tty/driver/serial)"
}

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
