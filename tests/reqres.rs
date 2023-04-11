use asynchronous_codec::{Event, LinesCodec, RecvSend, SendRecv};
use futures::channel::oneshot;
use futures_util::stream::SelectAll;
use futures_util::{AsyncReadExt, AsyncWriteExt, FutureExt, StreamExt};
use std::io::ErrorKind;
use std::iter::FromIterator;
use std::time::Duration;

#[tokio::test]
async fn smoke_recv_send() {
    let (client, mut server) = futures_ringbuf::Endpoint::pair(100, 100);
    server.write_all(b"hello\n").await.unwrap();

    let mut stream = RecvSend::new(client, LinesCodec).close_after_send();
    let (message, responder) = stream.next().await.unwrap().unwrap();

    assert_eq!(message, "hello\n");

    responder.respond("world\n".to_owned());
    let _ = stream.next().await;

    let mut buffer = Vec::new();
    server.read_to_end(&mut buffer).await.unwrap();

    assert_eq!(buffer, b"world\n");
}

#[tokio::test]
async fn smoke_send_recv() {
    let (client, mut server) = futures_ringbuf::Endpoint::pair(100, 100);

    let mut stream = SendRecv::new(client, LinesCodec, "hello\n".to_owned()).close_after_send();
    server.write_all(b"world\n").await.unwrap();

    let message = stream.next().await.unwrap().unwrap();
    let _ = stream.next().now_or_never().unwrap();

    let mut buffer = Vec::new();
    server.read_to_end(&mut buffer).await.unwrap();

    assert_eq!(buffer, b"hello\n");
    assert_eq!(message, "world\n");
}

#[tokio::test]
async fn no_response() {
    let (client, mut server) = futures_ringbuf::Endpoint::pair(100, 100);
    server.write_all(b"hello\n").await.unwrap();

    let mut stream = RecvSend::new(client, LinesCodec).close_after_send();
    let (message, responder) = stream.next().await.unwrap().unwrap();

    assert_eq!(message, "hello\n");

    // Drop without sending a response.
    drop(responder);
    assert_eq!(
        stream.next().await.unwrap().unwrap_err().kind(),
        ErrorKind::BrokenPipe
    );
}

#[tokio::test]
async fn runtime_driven() {
    let (client, mut server) = futures_ringbuf::Endpoint::pair(100, 100);
    server.write_all(b"hello\n").await.unwrap();

    let mut stream = RecvSend::new(client, LinesCodec);

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

        tx.send(stream).unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await; // simulate computation of response
    responder.respond("world\n".to_owned());
    rx.await.unwrap().close().await.unwrap();

    let mut buffer = Vec::new();
    server.read_to_end(&mut buffer).await.unwrap();

    assert_eq!(buffer, b"world\n");
}

#[tokio::test]
async fn select_all() {
    let (client1, mut server1) = futures_ringbuf::Endpoint::pair(100, 100);
    server1.write_all(b"hello1\n").await.unwrap();

    let (client2, mut server2) = futures_ringbuf::Endpoint::pair(100, 100);
    server2.write_all(b"hello2\n").await.unwrap();

    let mut streams = SelectAll::from_iter([
        RecvSend::new(client1, LinesCodec).close_after_send(),
        RecvSend::new(client2, LinesCodec).close_after_send(),
    ]);

    let (_request, responder) = streams.next().await.unwrap().unwrap();
    responder.respond("world1\n".to_owned());
    let (_request, responder) = streams.next().await.unwrap().unwrap();
    responder.respond("world2\n".to_owned());

    streams.next().await;

    drop(streams);

    let mut buffer1 = Vec::new();
    server1.read_to_end(&mut buffer1).await.unwrap();

    let mut buffer2 = Vec::new();
    server2.read_to_end(&mut buffer2).await.unwrap();

    assert_eq!(buffer1, b"world1\n");
    assert_eq!(buffer2, b"world2\n");
}
