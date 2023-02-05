use asynchronous_codec::{Event, LinesCodec, RecvSend};
use futures::channel::oneshot;
use futures::io::Cursor;
use futures_util::stream::SelectAll;
use futures_util::{FutureExt, StreamExt};
use std::iter::FromIterator;
use std::time::Duration;

#[test]
fn smoke() {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"hello\n");

    let mut stream = RecvSend::new(Cursor::new(&mut buffer), LinesCodec).close_after_send();
    let (message, slot) = stream.next().now_or_never().unwrap().unwrap().unwrap();

    assert_eq!(message, "hello\n");

    slot.fill("world\n".to_owned());
    let _ = stream.next().now_or_never().unwrap();

    assert_eq!(buffer, b"hello\nworld\n");
}

#[tokio::test]
async fn runtime_driven() {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"hello\n");

    let mut stream = RecvSend::new(Cursor::new(buffer), LinesCodec);

    let Event::NewRequest { req, res } =
        stream.next()
            .await
            .unwrap()
            .unwrap() else {
        panic!()
    };

    assert_eq!(req, "hello\n");

    let (rx, tx) = oneshot::channel();

    tokio::spawn(async move {
        let Event::Completed { stream } = stream.next().await.unwrap().unwrap() else { panic!() };

        rx.send(stream.into_inner()).unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await; // simulate computation of response
    res.fill("world\n".to_owned());
    assert_eq!(tx.await.unwrap(), b"hello\nworld\n");
}

#[tokio::test]
async fn close_after_send() {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"hello\n");

    let mut stream = RecvSend::new(Cursor::new(&mut buffer), LinesCodec).close_after_send();

    let (_, slot) = stream.next().await.unwrap().unwrap();

    slot.fill("world\n".to_owned());

    stream.next().await;

    assert_eq!(buffer, b"hello\nworld\n");

    // TODO: How can we assert that closing works properly?
}

#[tokio::test]
async fn select_all() {
    let mut buffer1 = Vec::new();
    buffer1.extend_from_slice(b"hello\n");
    let mut buffer2 = Vec::new();
    buffer2.extend_from_slice(b"hello\n");

    let mut streams = SelectAll::from_iter([
        RecvSend::new(Cursor::new(&mut buffer1), LinesCodec).close_after_send(),
        RecvSend::new(Cursor::new(&mut buffer2), LinesCodec).close_after_send(),
    ]);

    let (_, slot) = streams.next().await.unwrap().unwrap();
    slot.fill("world1\n".to_owned());
    let (_, slot) = streams.next().await.unwrap().unwrap();
    slot.fill("world2\n".to_owned());

    streams.next().await;

    drop(streams);

    assert_eq!(buffer1, b"hello\nworld1\n");
    assert_eq!(buffer2, b"hello\nworld2\n");

    // TODO: How can we assert that closing works properly?
}
