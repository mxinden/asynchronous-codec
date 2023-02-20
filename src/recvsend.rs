use crate::{Decoder, Encoder, Framed};
use futures_util::io::{AsyncRead, AsyncWrite};
use futures_util::{SinkExt, Stream, StreamExt};
use std::marker::PhantomData;
use std::{
    fmt, io, mem,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

/// The recv-send message pattern.
///
/// This struct implements a request-response message pattern for the receiving side.
/// It will attempt to first read a message from the stream and then send a response.
///
/// This struct implements [`Stream`] but each instance will only ever emit one item. The reason
/// this implements [`Stream`] instead of [`Future`] is that a [`Future`] will not be polled after
/// it has resolved. This struct however needs to do more work after emitting the request: Sending the response.
///
/// This component works really well when used together with [`SelectAll`](futures::stream::SelectAll).
pub struct RecvSend<S, C, B>
where
    C: Encoder,
{
    inner: RecvSendState<S, C>,
    behaviour: PhantomData<B>,
}

impl<S, C> RecvSend<S, C, ReturnStream>
where
    C: Encoder + Decoder,
    S: AsyncRead + AsyncWrite,
{
    /// TODO
    pub fn new(stream: S, codec: C) -> Self {
        Self {
            inner: RecvSendState::Receiving {
                framed: Framed::new(stream, codec),
            },
            behaviour: Default::default(),
        }
    }

    /// Reconfigure this future to close the stream after the message has been sent instead of returning it.
    pub fn close_after_send(self) -> RecvSend<S, C, CloseStream> {
        RecvSend {
            inner: self.inner,
            behaviour: Default::default(),
        }
    }
}

enum RecvSendState<S, C: Encoder> {
    Receiving { framed: Framed<S, C> },
    Sending(Sending<S, C>),
    Flushing { framed: Framed<S, C> },
    Closing { framed: Framed<S, C> },
    Done,
    Poisoned,
}

impl<S, C, Req, Res, E> Stream for RecvSend<S, C, CloseStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Item = Res, Error = E> + Decoder<Item = Req, Error = E>,
    E: From<io::Error> + std::error::Error + Send + Sync + 'static,
{
    type Item = Result<(Req, Responder<Res>), io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            match mem::replace(&mut this.inner, RecvSendState::Poisoned) {
                RecvSendState::Receiving { mut framed } => {
                    let request = match framed
                        .poll_next_unpin(cx)
                        .map_err(Error::Recv)
                        .map_err(into_io_error)?
                    {
                        Poll::Ready(Some(request)) => request,
                        Poll::Ready(None) => {
                            return Poll::Ready(Some(Err(io::Error::from(
                                io::ErrorKind::UnexpectedEof,
                            ))));
                        }
                        Poll::Pending => {
                            this.inner = RecvSendState::Receiving { framed };
                            return Poll::Pending;
                        }
                    };

                    let shared = Arc::new(Mutex::new(Shared::default()));
                    this.inner = RecvSendState::Sending(Sending {
                        framed,
                        shared: shared.clone(),
                    });

                    let responder = Responder { shared };

                    return Poll::Ready(Some(Ok((request, responder))));
                }
                RecvSendState::Sending(mut sending) => match sending.poll(cx)? {
                    Poll::Ready(()) => {
                        this.inner = RecvSendState::Flushing {
                            framed: sending.framed,
                        };
                    }
                    Poll::Pending => {
                        this.inner = RecvSendState::Sending(sending);
                        return Poll::Pending;
                    }
                },
                RecvSendState::Flushing { mut framed } => {
                    match framed
                        .poll_flush_unpin(cx)
                        .map_err(Error::Recv)
                        .map_err(into_io_error)?
                    {
                        Poll::Ready(()) => {
                            this.inner = RecvSendState::Closing { framed };
                        }
                        Poll::Pending => {
                            this.inner = RecvSendState::Flushing { framed };
                            return Poll::Pending;
                        }
                    }
                }
                RecvSendState::Closing { mut framed } => {
                    match framed
                        .poll_close_unpin(cx)
                        .map_err(Error::Recv)
                        .map_err(into_io_error)?
                    {
                        Poll::Ready(()) => {
                            this.inner = RecvSendState::Done;
                            continue;
                        }
                        Poll::Pending => {
                            this.inner = RecvSendState::Closing { framed };
                            return Poll::Pending;
                        }
                    };
                }
                RecvSendState::Done => {
                    return Poll::Ready(None);
                }
                RecvSendState::Poisoned => {
                    unreachable!()
                }
            }
        }
    }
}

fn into_io_error<E>(e: Error<E>) -> io::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    io::Error::new(io::ErrorKind::Other, e)
}

/// TODO
pub enum Event<Req, Res, S> {
    /// TODO
    NewRequest {
        /// TODO
        request: Req,
        /// TODO
        responder: Responder<Res>,
    },
    /// TODO
    Completed {
        /// TODO
        stream: S,
    },
}

impl<S, C, Req, Res, E> Stream for RecvSend<S, C, ReturnStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Item = Res, Error = E> + Decoder<Item = Req, Error = E>,
    E: std::error::Error + Send + Sync + 'static,
{
    type Item = Result<Event<Req, Res, S>, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            match mem::replace(&mut this.inner, RecvSendState::Poisoned) {
                RecvSendState::Receiving { mut framed } => {
                    let request = match framed
                        .poll_next_unpin(cx)
                        .map_err(Error::Recv)
                        .map_err(into_io_error)?
                    {
                        Poll::Ready(Some(request)) => request,
                        Poll::Ready(None) => {
                            return Poll::Ready(Some(Err(io::Error::from(
                                io::ErrorKind::UnexpectedEof,
                            ))));
                        }
                        Poll::Pending => {
                            this.inner = RecvSendState::Receiving { framed };
                            return Poll::Pending;
                        }
                    };

                    let shared = Arc::new(Mutex::new(Shared {
                        waker: Some(cx.waker().clone()),
                        ..Shared::default()
                    }));
                    this.inner = RecvSendState::Sending(Sending {
                        framed,
                        shared: shared.clone(),
                    });

                    let responder = Responder { shared };

                    return Poll::Ready(Some(Ok(Event::NewRequest { request, responder })));
                }
                RecvSendState::Sending(mut sending) => match sending.poll(cx)? {
                    Poll::Ready(()) => {
                        this.inner = RecvSendState::Flushing {
                            framed: sending.framed,
                        };
                    }
                    Poll::Pending => {
                        this.inner = RecvSendState::Sending(sending);
                        return Poll::Pending;
                    }
                },
                RecvSendState::Flushing { mut framed } => {
                    match framed
                        .poll_flush_unpin(cx)
                        .map_err(Error::Recv)
                        .map_err(into_io_error)?
                    {
                        Poll::Ready(()) => {
                            this.inner = RecvSendState::Done;
                            return Poll::Ready(Some(Ok(Event::Completed {
                                stream: framed.into_parts().io,
                            })));
                        }
                        Poll::Pending => {
                            this.inner = RecvSendState::Flushing { framed };
                            return Poll::Pending;
                        }
                    }
                }
                RecvSendState::Closing { .. } => {
                    unreachable!("We never go into `Closing`")
                }
                RecvSendState::Done => {
                    this.inner = RecvSendState::Done;
                    return Poll::Ready(None);
                }
                RecvSendState::Poisoned => {
                    unreachable!()
                }
            }
        }
    }
}

/// Marker type for a [`SendingResponse`] future that will return the stream back to the user after the message has been sent.
///
/// This may be useful if multiple request-response exchanges should happen on the same stream.
pub enum ReturnStream {}

/// Marker type for a [`SendingResponse`] future that will close the stream after the message has been sent.
pub enum CloseStream {}

#[derive(Debug)]
enum Error<Enc> {
    Recv(Enc),
    Send(Enc),
}

impl<Enc> fmt::Display for Error<Enc> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Recv(_) => write!(f, "failed to recv on stream"),
            Error::Send(_) => write!(f, "failed to send on stream"),
        }
    }
}

impl<Enc> std::error::Error for Error<Enc>
where
    Enc: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Recv(inner) => Some(inner),
            Error::Send(inner) => Some(inner),
        }
    }
}

/// Responder for corresponding request.
///
/// Can be just dropped if response if not necessary for this request.
pub struct Responder<Response> {
    shared: Arc<Mutex<Shared<Response>>>,
}

impl<Response> Drop for Responder<Response> {
    fn drop(&mut self) {
        if let Some(waker) = self.shared.lock().unwrap().waker.take() {
            waker.wake();
        }
    }
}

impl<Response> Responder<Response> {
    /// Send response.
    ///
    /// This consumes the responder because a response can only be sent once.
    /// The actual IO for sending the response happens in the [`SendingResponse`] future.
    pub fn respond(self, response: Response) {
        self.shared.lock().unwrap().message = Some(response);
    }
}

struct Sending<S, C>
where
    C: Encoder,
{
    framed: Framed<S, C>,
    shared: Arc<Mutex<Shared<C::Item>>>,
}

impl<S, C, Req, Res, E> Sending<S, C>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Item = Res, Error = E> + Decoder<Item = Req, Error = E>,
    E: std::error::Error + Send + Sync + 'static,
{
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        futures_util::ready!(self.framed.poll_ready_unpin(cx).map_err(Error::Send))
            .map_err(into_io_error)?;

        let response = {
            let mut guard = self.shared.lock().unwrap();

            match guard.message.take() {
                Some(response) => response,
                None => {
                    return if guard.waker.replace(cx.waker().clone()).is_none() {
                        // Responder was dropped before sending any value, nothing else left to do here.
                        Poll::Ready(Ok(()))
                    } else {
                        Poll::Pending
                    };
                }
            }
        };

        self.framed
            .start_send_unpin(response)
            .map_err(Error::Send)
            .map_err(into_io_error)?;

        Poll::Ready(Ok(()))
    }
}

struct Shared<M> {
    message: Option<M>,
    waker: Option<Waker>,
}

impl<M> Default for Shared<M> {
    fn default() -> Self {
        Self {
            message: None,
            waker: None,
        }
    }
}
