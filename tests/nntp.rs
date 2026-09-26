use rustvn::{
    model::{Security, Server},
    nntp::{Cancel, Client},
    store::Store,
    worker::{Action, Update, execute},
};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
    time::Duration,
};
use zeroize::Zeroizing;

fn server(script: Vec<(&'static str, Vec<u8>)>) -> (Server, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = Server {
        name: "Local test fixture".into(),
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
        security: Security::LocalPlain,
        ..Server::default()
    };
    let join = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream.write_all(b"200 mock ready\r\n").unwrap();
        let mut reader = BufReader::new(stream);
        for (expected, response) in script {
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert_eq!(request.trim_end(), expected);
            assert!(!request.starts_with("POST"));
            reader.get_mut().write_all(&response).unwrap();
        }
    });
    (config, join)
}
fn handshake() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "CAPABILITIES",
            b"101 caps\r\nVERSION 2\r\nREADER\r\n.\r\n".to_vec(),
        ),
        ("MODE READER", b"201 read only\r\n".to_vec()),
    ]
}

#[test]
fn real_socket_over_fallback_holes_and_dot_unstuffing() {
    let mut script = handshake();
    script.extend([
        ("LIST ACTIVE",b"215 groups\r\nfj.test 3 1 y\r\n.\r\n".to_vec()),
        ("GROUP fj.test",b"211 2 1 3 fj.test\r\n".to_vec()),
        ("OVER 1-3",b"500 unsupported\r\n".to_vec()),
        ("XOVER 1-3",b"224 overview\r\n1\t=?UTF-8?B?5pel5pys6Kqe?=\tAlice\t26 Sep 2026 10:00:00 +0900\t<1@test>\t\t100\t4\r\n3\tRe: test\tBob\t26 Sep 2026 11:00:00 +0900\t<3@test>\t<1@test>\t100\t4\r\n.\r\n".to_vec()),
        ("ARTICLE <1@test>",b"220 1 <1@test> article\r\nMessage-ID: <1@test>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n..dot\r\nlast\r\n.\r\n".to_vec()),
    ]);
    let (s, join) = server(script);
    let mut c = Client::connect(&s, "", Cancel::default()).unwrap();
    assert_eq!(c.groups().unwrap()[0].name, "fj.test");
    assert_eq!(c.select_group("fj.test").unwrap(), (1, 3));
    let a = c.overview(1, 3).unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].subject, "日本語");
    assert!(
        String::from_utf8(c.article("<1@test>").unwrap())
            .unwrap()
            .contains("\r\n.dot\r\n")
    );
    join.join().unwrap();
}
#[test]
fn truncated_multiline_is_not_success() {
    let mut script = handshake();
    script.push(("LIST ACTIVE", b"215 list\r\nfj.test 1 1 y\r\n".to_vec()));
    let (s, j) = server(script);
    let mut c = Client::connect(&s, "", Cancel::default()).unwrap();
    assert!(c.groups().unwrap_err().to_string().contains("切断"));
    j.join().unwrap();
}
#[test]
fn expired_and_mismatched_article_do_not_enter_cache() {
    for response in [
        b"430 missing\r\n".to_vec(),
        b"220 1 <one> article\r\nMessage-ID: <other>\r\n\r\nwrong\r\n.\r\n".to_vec(),
    ] {
        let mut script = handshake();
        script.push(("ARTICLE <one>", response));
        let (s, j) = server(script);
        let d = tempfile::tempdir().unwrap();
        let mut db = Store::open(&d.path().join("db")).unwrap();
        db.save_server(&s).unwrap();
        let r = execute(
            &mut db,
            Action::Body {
                server: s.clone(),
                password: Zeroizing::new(String::new()),
                group: "fj.test".into(),
                id: "<one>".into(),
                network: true,
                charset: None,
            },
            &Cancel::default(),
        );
        assert!(r.is_err());
        assert!(db.body(&s.id, "<one>").unwrap().is_none());
        j.join().unwrap();
    }
}
#[test]
fn offline_body_roundtrip_without_network() {
    let d = tempfile::tempdir().unwrap();
    let mut db = Store::open(&d.path().join("db")).unwrap();
    let s = Server::default();
    db.save_server(&s).unwrap();
    db.save_body(
        &s.id,
        "<one>",
        "Subject: test\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n日本語".as_bytes(),
        "日本語",
    )
    .unwrap();
    let r = execute(
        &mut db,
        Action::Body {
            server: s,
            password: Zeroizing::new(String::new()),
            group: "fj.test".into(),
            id: "<one>".into(),
            network: false,
            charset: None,
        },
        &Cancel::default(),
    )
    .unwrap();
    assert!(matches!(r,Update::Body(doc,_,_) if doc.body=="日本語"));
}
#[test]
fn cannot_inject_or_send_credentials_in_cleartext() {
    let mut s = Server {
        security: Security::LocalPlain,
        name: "Local test fixture".into(),
        host: "127.0.0.1".into(),
        username: "user".into(),
        ..Server::default()
    };
    assert!(Client::connect(&s, "secret", Cancel::default()).is_err());
    s.username.clear();
    s.host = "example.com".into();
    assert!(Client::connect(&s, "", Cancel::default()).is_err());
    s.host = "127.0.0.1\r\nPOST".into();
    assert!(Client::connect(&s, "", Cancel::default()).is_err());
}
#[test]
fn cancellation_interrupts_blocked_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let peer = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.write_all(b"200 ready\r\n").unwrap();
        let mut reader = BufReader::new(s);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        tx.send(()).unwrap();
        let mut rest = String::new();
        let _ = reader.read_line(&mut rest);
    });
    let s = Server {
        name: "Local test fixture".into(),
        host: "127.0.0.1".into(),
        port,
        security: Security::LocalPlain,
        ..Server::default()
    };
    let cancel = Cancel::default();
    let clone = cancel.clone();
    let (done, result) = mpsc::channel();
    let client = thread::spawn(move || {
        done.send(Client::connect(&s, "", clone).is_err()).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    cancel.cancel();
    assert!(result.recv_timeout(Duration::from_secs(2)).unwrap());
    client.join().unwrap();
    peer.join().unwrap();
}
#[test]
fn starttls_rejection_never_authenticates_or_downgrades() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let j = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        s.write_all(b"200 ready\r\n").unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert_eq!(line, "STARTTLS\r\n");
        r.get_mut().write_all(b"580 unavailable\r\n").unwrap();
        line.clear();
        assert_eq!(r.read_line(&mut line).unwrap(), 0);
    });
    let s = Server {
        name: "Local test fixture".into(),
        host: "127.0.0.1".into(),
        port,
        security: Security::StartTls,
        username: "user".into(),
        ..Server::default()
    };
    assert!(Client::connect(&s, "secret", Cancel::default()).is_err());
    j.join().unwrap();
}
#[test]
fn implicit_tls_does_not_accept_plaintext() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let j = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.write_all(b"200 this is not TLS\r\n").unwrap();
    });
    let s = Server {
        name: "Local test fixture".into(),
        host: "127.0.0.1".into(),
        port,
        username: "user".into(),
        ..Server::default()
    };
    assert!(Client::connect(&s, "secret", Cancel::default()).is_err());
    j.join().unwrap();
}
