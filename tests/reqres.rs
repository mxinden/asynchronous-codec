use asynchronous_codec::{Event, LinesCodec, RecvSend};
use futures::channel::oneshot;
use futures::io::Cursor;
use futures_util::stream::SelectAll;
use futures_util::{FutureExt, StreamExt};
use std::iter::FromIterator;
use std::time::Duration;

#[test]
fn smoke() {
    // Emulating duplex stream, both reads and writes happen on the same buffer for convenience.
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"hello\n");

    let mut stream = RecvSend::new(Cursor::new(&mut buffer), LinesCodec).close_after_send();
    let (message, responder) = stream.next().now_or_never().unwrap().unwrap().unwrap();

    assert_eq!(message, "hello\n");

    responder.respond("world\n".to_owned());
    let _ = stream.next().now_or_never().unwrap();

    assert_eq!(buffer, b"hello\nworld\n");
}

#[test]
fn no_response() {
    // Emulating duplex stream, both reads and writes happen on the same buffer for convenience.
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"hello\n");

    let mut stream = RecvSend::new(Cursor::new(&mut buffer), LinesCodec).close_after_send();
    let (message, responder) = stream.next().now_or_never().unwrap().unwrap().unwrap();

    assert_eq!(message, "hello\n");

    // Drop without sending a response.
    drop(responder);
    let _ = stream.next().now_or_never().unwrap();

    assert_eq!(buffer, b"hello\n");
}

#[tokio::test]
async fn runtime_driven() {
    // Emulating duplex stream, both reads and writes happen on the same buffer for convenience.
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"hello\n");

    let mut stream = RecvSend::new(Cursor::new(buffer), LinesCodec);

    let Event::NewRequest { request, responder } =
        stream.next()
            .await
            .unwrap()
            .unwrap() else {
        panic!()
    };

    assert_eq!(request, "hello\n");

    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        let Event::Completed { stream } = stream.next().await.unwrap().unwrap() else { panic!() };

        tx.send(stream.into_inner()).unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await; // simulate computation of response
    responder.respond("world\n".to_owned());
    assert_eq!(rx.await.unwrap(), b"hello\nworld\n");
}

#[tokio::test]
async fn close_after_send() {
    // Emulating duplex stream, both reads and writes happen on the same buffer for convenience.
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"hello\n");

    let mut stream = RecvSend::new(Cursor::new(&mut buffer), LinesCodec).close_after_send();

    let (_request, responder) = stream.next().await.unwrap().unwrap();

    responder.respond("world\n".to_owned());

    stream.next().await;

    assert_eq!(buffer, b"hello\nworld\n");

    // TODO: How can we assert that closing works properly?
}

#[tokio::test]
async fn select_all() {
    // Emulating duplex stream, both reads and writes happen on the same buffer for convenience.
    let mut buffer1 = Vec::new();
    buffer1.extend_from_slice(b"hello\n");
    let mut buffer2 = Vec::new();
    buffer2.extend_from_slice(b"hello\n");

    let mut streams = SelectAll::from_iter([
        RecvSend::new(Cursor::new(&mut buffer1), LinesCodec).close_after_send(),
        RecvSend::new(Cursor::new(&mut buffer2), LinesCodec).close_after_send(),
    ]);

    let (_request, responder) = streams.next().await.unwrap().unwrap();
    responder.respond("world1\n".to_owned());
    let (_request, responder) = streams.next().await.unwrap().unwrap();
    responder.respond("world2\n".to_owned());

    streams.next().await;

    drop(streams);

    assert_eq!(buffer1, b"hello\nworld1\n");
    assert_eq!(buffer2, b"hello\nworld2\n");

    // TODO: How can we assert that closing works properly?
}
