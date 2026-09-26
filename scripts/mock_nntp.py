#!/usr/bin/env python3
"""Local, read-only NNTP fixture. No credentials or external network access."""
import argparse
import socketserver

ARTICLES = {
    1: ("Rustでニュースリーダーを作ろう", "", "久しぶりにfjを読むための、ローカル試験用記事です。\n\n日本語の記事と既読状態を確認できます。\n.ドットから始まる行も表示できます。"),
    3: ("Re: Rustでニュースリーダーを作ろう", "<1@rustvn.test>", "> 日本語の記事と既読状態を確認できます。\n\n返信のスレッド表示も確認してみましょう。"),
    5: ("日本語とオフライン表示", "", "本文を一度開いてからオフラインにしてください。\nアプリを再起動しても保存済みの記事を読めます。"),
}


def article(number):
    subject, refs, body = ARTICLES[number]
    return (f"Subject: {subject}\r\nFrom: Demo <demo@example.invalid>\r\n"
            f"Date: Sat, 26 Sep 2026 10:0{number}:00 +0900\r\n"
            f"Message-ID: <{number}@rustvn.test>\r\nReferences: {refs}\r\n"
            "Newsgroups: fj.test\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n"
            + body.replace("\n", "\r\n")).encode()


class Handler(socketserver.StreamRequestHandler):
    def reply(self, line):
        self.wfile.write(line.encode() + b"\r\n")

    def block(self, lines):
        for line in lines:
            self.wfile.write((b"." if line.startswith(b".") else b"") + line + b"\r\n")
        self.reply(".")

    def handle(self):
        self.connection.settimeout(30)
        self.reply("201 rustVN local read-only fixture")
        while request := self.rfile.readline(4096):
            words = request.decode("ascii", errors="replace").strip().split()
            if not words:
                continue
            command = words[0].upper()
            if command == "CAPABILITIES":
                self.reply("101 capabilities")
                self.block([b"VERSION 2", b"READER", b"OVER", b"LIST ACTIVE"])
            elif command == "MODE":
                self.reply("201 read only")
            elif command == "LIST":
                self.reply("215 groups")
                self.block([b"fj.test 5 1 n", b"fj.empty 0 1 n"])
            elif command == "GROUP":
                self.reply("211 3 1 5 fj.test" if words[-1] == "fj.test" else "211 0 1 0 fj.empty")
            elif command in ("OVER", "XOVER"):
                start, end = map(int, words[-1].split("-"))
                self.reply("224 overview")
                self.block([
                    (f"{n}\t{s}\tDemo\t26 Sep 2026 10:0{n}:00 +0900\t<{n}@rustvn.test>\t{refs}\t400\t10").encode()
                    for n, (s, refs, _) in ARTICLES.items() if start <= n <= end
                ])
            elif command == "ARTICLE":
                found = next((n for n in ARTICLES if words[-1] == f"<{n}@rustvn.test>"), None)
                if found is None:
                    self.reply("430 no article")
                else:
                    self.reply(f"220 {found} <{found}@rustvn.test>")
                    self.block(article(found).split(b"\r\n"))
            elif command == "QUIT":
                self.reply("205 goodbye")
                return
            else:
                self.reply("500 unsupported (read only)")


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8119)
    args = parser.parse_args()
    with Server(("127.0.0.1", args.port), Handler) as server:
        print(f"Local NNTP fixture: 127.0.0.1:{server.server_address[1]} (no authentication)", flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass
