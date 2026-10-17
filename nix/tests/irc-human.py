"""A person on IRC, for the e2e test: connects to SERVER as NICK, logs in to
the account NICK with SASL if given a password, asks for message-tags so
client tags like replies and reactions show up, and joins channels. Every
line written to FIFO is sent as a raw IRC line, and every line received is
appended to LOG. Exits when the server closes the connection."""

import argparse
import base64
import os
import socket
import threading


def relay(fifo, send):
    # Opened read-write, so it never sees EOF and writers never block.
    with os.fdopen(os.open(fifo, os.O_RDWR), encoding="utf-8") as lines:
        for line in lines:
            send(line.rstrip("\n"))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("server", help="host:port, spoken to in plaintext")
    parser.add_argument("nick")
    parser.add_argument("--password", help="SASL PLAIN password for NICK")
    parser.add_argument("--join", action="append", default=[])
    parser.add_argument("--fifo", required=True)
    parser.add_argument("--log", required=True)
    args = parser.parse_args()

    host, port = args.server.rsplit(":", 1)
    sock = socket.create_connection((host, int(port)))
    sending = threading.Lock()

    def send(line):
        with sending:
            sock.sendall(line.encode() + b"\r\n")

    send("CAP REQ :message-tags")
    if args.password:
        plain = f"{args.nick}\0{args.nick}\0{args.password}"
        send("CAP REQ :sasl")
        send("AUTHENTICATE PLAIN")
        send("AUTHENTICATE " + base64.b64encode(plain.encode()).decode())
    send("CAP END")
    send(f"NICK {args.nick}")
    send(f"USER {args.nick} 0 * :{args.nick}")

    os.mkfifo(args.fifo)
    threading.Thread(target=relay, args=(args.fifo, send), daemon=True).start()

    received = sock.makefile(encoding="utf-8", newline="\r\n")
    with open(args.log, "a", encoding="utf-8", buffering=1) as log:
        for line in received:
            line = line.rstrip("\r\n")
            log.write(line + "\n")
            if line.startswith("@"):
                line = line.split(" ", 1)[1]
            words = line.split(" ")
            if words[0] == "PING":
                send("PONG " + words[1])
            elif words[1:2] == ["001"]:
                for channel in args.join:
                    send(f"JOIN {channel}")


if __name__ == "__main__":
    main()
