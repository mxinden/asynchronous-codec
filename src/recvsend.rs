use crate::message_patterns::{CloseStream, Error, ReturnStream, Sending};
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

    /// Abort this message exchange, immediately freeing all resources.
    pub fn abort(&mut self) {
        self.inner = RecvSendState::Done;
    }
}

enum RecvSendState<S, C: Encoder> {
    Receiving { framed: Framed<S, C> },
    Waiting(Waiting<C>, Framed<S, C>),
    Sending(Sending<S, C>),
    Flushing { framed: Framed<S, C> },
    Closing { framed: Framed<S, C> },
    Done,
    Poisoned,
}

impl<S, C: Encoder> fmt::Debug for RecvSendState<S, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecvSendState::Receiving { .. } => f
                .debug_struct("RecvSendState::Receiving")
                .finish_non_exhaustive(),
            RecvSendState::Waiting(_, _) => f
                .debug_struct("RecvSendState::Waiting")
                .finish_non_exhaustive(),
            RecvSendState::Sending(_) => f
                .debug_struct("RecvSendState::Sending")
                .finish_non_exhaustive(),
            RecvSendState::Flushing { .. } => f
                .debug_struct("RecvSendState::Flushing")
                .finish_non_exhaustive(),
            RecvSendState::Closing { .. } => f
                .debug_struct("RecvSendState::Closing")
                .finish_non_exhaustive(),
            RecvSendState::Done => f
                .debug_struct("RecvSendState::Done")
                .finish_non_exhaustive(),
            RecvSendState::Poisoned => f
                .debug_struct("RecvSendState::Poisoned")
                .finish_non_exhaustive(),
        }
    }
}

impl<S, C, Req, Res, E> Stream for RecvSend<S, C, CloseStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Item = Res, Error = E> + Decoder<Item = Req, Error = E>,
    E: From<io::Error> + std::error::Error + Send + Sync + Unpin + 'static,
    Res: Unpin,
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
                    this.inner = RecvSendState::Waiting(
                        Waiting {
                            shared: shared.clone(),
                        },
                        framed,
                    );

                    let responder = Responder { shared };

                    return Poll::Ready(Some(Ok((request, responder))));
                }
                RecvSendState::Waiting(mut waiting, framed) => {
                    let response = match waiting.poll(cx) {
                        Poll::Ready(Ok(response)) => response,
                        Poll::Ready(Err(e)) => {
                            this.inner = RecvSendState::Closing { framed };
                            return Poll::Ready(Some(Err(e)));
                        }
                        Poll::Pending => {
                            this.inner = RecvSendState::Waiting(waiting, framed);
                            return Poll::Pending;
                        }
                    };
                    this.inner = RecvSendState::Sending(Sending {
                        framed,
                        message: Some(response),
                    });
                }
                RecvSendState::Sending(mut sending) => match sending.poll(cx) {
                    Poll::Ready(Ok(())) => {
                        this.inner = RecvSendState::Flushing {
                            framed: sending.framed,
                        };
                    }
                    Poll::Ready(Err(e)) => {
                        this.inner = RecvSendState::Closing {
                            framed: sending.framed,
                        };
                        return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
                    }
                    Poll::Pending => {
                        this.inner = RecvSendState::Sending(sending);
                        return Poll::Pending;
                    }
                },
                RecvSendState::Flushing { mut framed } => match framed.poll_flush_unpin(cx) {
                    Poll::Ready(Ok(())) => {
                        this.inner = RecvSendState::Closing { framed };
                    }
                    Poll::Ready(Err(e)) => {
                        this.inner = RecvSendState::Done;
                        return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
                    }
                    Poll::Pending => {
                        this.inner = RecvSendState::Flushing { framed };
                        return Poll::Pending;
                    }
                },
                RecvSendState::Closing { mut framed } => {
                    match framed.poll_close_unpin(cx) {
                        Poll::Ready(Ok(())) => {
                            this.inner = RecvSendState::Done;
                            continue;
                        }
                        Poll::Ready(Err(e)) => {
                            this.inner = RecvSendState::Done;
                            return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
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

pub(crate) fn into_io_error<E>(e: Error<E>) -> io::Error
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
    E: std::error::Error + Send + Sync + Unpin + 'static,
    Res: Unpin,
{
    type Item = Result<Event<Req, Res, S>, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            match mem::replace(&mut this.inner, RecvSendState::Poisoned) {
                RecvSendState::Receiving { mut framed } => {
                    let request = match framed.poll_next_unpin(cx) {
                        Poll::Ready(Some(Ok(request))) => request,
                        Poll::Ready(Some(Err(e))) => {
                            this.inner = RecvSendState::Done;
                            return Poll::Ready(Some(Err(into_io_error(Error::Recv(e)))));
                        }
                        Poll::Ready(None) => {
                            this.inner = RecvSendState::Done;
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
                    this.inner = RecvSendState::Waiting(
                        Waiting {
                            shared: shared.clone(),
                        },
                        framed,
                    );

                    let responder = Responder { shared };

                    return Poll::Ready(Some(Ok(Event::NewRequest { request, responder })));
                }
                RecvSendState::Waiting(mut waiting, framed) => {
                    let response = match waiting.poll(cx) {
                        Poll::Ready(Ok(response)) => response,
                        Poll::Ready(Err(e)) => {
                            this.inner = RecvSendState::Done;
                            return Poll::Ready(Some(Err(e)));
                        }
                        Poll::Pending => {
                            this.inner = RecvSendState::Waiting(waiting, framed);
                            return Poll::Pending;
                        }
                    };
                    this.inner = RecvSendState::Sending(Sending {
                        framed,
                        message: Some(response),
                    });
                }
                RecvSendState::Sending(mut sending) => match sending.poll(cx) {
                    Poll::Ready(Ok(())) => {
                        this.inner = RecvSendState::Flushing {
                            framed: sending.framed,
                        };
                    }
                    Poll::Ready(Err(e)) => {
                        this.inner = RecvSendState::Done;
                        return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
                    }
                    Poll::Pending => {
                        this.inner = RecvSendState::Sending(sending);
                        return Poll::Pending;
                    }
                },
                RecvSendState::Flushing { mut framed } => match framed.poll_flush_unpin(cx) {
                    Poll::Ready(Ok(())) => {
                        this.inner = RecvSendState::Done;
                        return Poll::Ready(Some(Ok(Event::Completed {
                            stream: framed.into_parts().io,
                        })));
                    }
                    Poll::Ready(Err(e)) => {
                        this.inner = RecvSendState::Done;
                        return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
                    }
                    Poll::Pending => {
                        this.inner = RecvSendState::Flushing { framed };
                        return Poll::Pending;
                    }
                },
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

/// The slot for the response to be sent on the stream.
#[derive(Debug)]
pub struct Responder<Res> {
    shared: Arc<Mutex<Shared<Res>>>,
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

#[derive(Debug)]
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

struct Waiting<C>
where
    C: Encoder,
{
    shared: Arc<Mutex<Shared<C::Item>>>,
}

impl<C, Req, Res, E> Waiting<C>
where
    C: Encoder<Item = Res, Error = E> + Decoder<Item = Req, Error = E>,
    E: std::error::Error + Send + Sync + 'static,
{
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<Res, io::Error>> {
        let mut guard = self.shared.lock().unwrap();

        let response = match guard.message.take() {
            Some(response) => response,
            None => {
                return if guard.waker.replace(cx.waker().clone()).is_none() {
                    Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
                } else {
                    Poll::Pending
                };
            }
        };

        Poll::Ready(Ok(response))
    }
}
