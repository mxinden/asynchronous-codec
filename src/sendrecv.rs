use crate::message_patterns::{Error, Sending};
use crate::{CloseStream, Decoder, Encoder, Framed, ReturnStream};
use futures_util::io::{AsyncRead, AsyncWrite};
use futures_util::{SinkExt, Stream, StreamExt};
use std::marker::PhantomData;
use std::{
    io, mem,
    pin::Pin,
    task::{Context, Poll},
};

/// The send-recv message pattern.
///
/// This struct implements a request-response message pattern for the receiving side.
/// It will attempt to first read a message from the stream and then send a response.
///
/// This struct implements [`Stream`] but each instance will only ever emit one item. The reason
/// this implements [`Stream`] instead of [`Future`] is that a [`Future`] will not be polled after
/// it has resolved. This struct however needs to do more work after emitting the request: Sending the response.
///
/// This component works really well when used together with [`SelectAll`](futures::stream::SelectAll).
pub struct SendRecv<S, C, B>
where
    C: Encoder,
{
    inner: SendRecvState<S, C>,
    behaviour: PhantomData<B>,
}

impl<S, C> SendRecv<S, C, ReturnStream>
where
    C: Encoder + Decoder,
    S: AsyncRead + AsyncWrite,
{
    /// TODO
    pub fn new(stream: S, codec: C, message: <C as Encoder>::Item) -> Self {
        Self {
            inner: SendRecvState::Sending(Sending {
                framed: Framed::new(stream, codec),
                message: Some(message),
            }),
            behaviour: Default::default(),
        }
    }

    /// Reconfigure this future to close the stream after the message has been sent instead of returning it.
    pub fn close_after_send(self) -> SendRecv<S, C, CloseStream> {
        SendRecv {
            inner: self.inner,
            behaviour: Default::default(),
        }
    }
}

enum SendRecvState<S, C: Encoder> {
    Sending(Sending<S, C>),
    Flushing { framed: Framed<S, C> },
    Receiving { framed: Framed<S, C> },
    Closing { framed: Framed<S, C> },
    Done,
    Poisoned,
}

impl<S, C, Req, Res, E> Stream for SendRecv<S, C, CloseStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Item = Req, Error = E> + Decoder<Item = Res, Error = E>,
    E: From<io::Error> + std::error::Error + Send + Sync + 'static,
    Req: Unpin,
{
    type Item = Result<Res, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            match mem::replace(&mut this.inner, SendRecvState::Poisoned) {
                SendRecvState::Sending(mut sending) => match sending.poll(cx) {
                    Poll::Ready(Ok(())) => {
                        this.inner = SendRecvState::Flushing {
                            framed: sending.framed,
                        };
                    }
                    Poll::Ready(Err(e)) => {
                        this.inner = SendRecvState::Done;
                        return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
                    }
                    Poll::Pending => {
                        this.inner = SendRecvState::Sending(sending);
                        return Poll::Pending;
                    }
                },
                SendRecvState::Flushing { mut framed } => match framed.poll_flush_unpin(cx) {
                    Poll::Ready(Ok(())) => {
                        this.inner = SendRecvState::Receiving { framed };
                    }
                    Poll::Ready(Err(e)) => {
                        this.inner = SendRecvState::Done;
                        return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
                    }
                    Poll::Pending => {
                        this.inner = SendRecvState::Flushing { framed };
                        return Poll::Pending;
                    }
                },
                SendRecvState::Receiving { mut framed } => {
                    let response = match framed.poll_next_unpin(cx) {
                        Poll::Ready(Some(Ok(request))) => request,
                        Poll::Ready(Some(Err(e))) => {
                            this.inner = SendRecvState::Done;
                            return Poll::Ready(Some(Err(into_io_error(Error::Recv(e)))));
                        }
                        Poll::Ready(None) => {
                            return Poll::Ready(Some(Err(io::Error::from(
                                io::ErrorKind::UnexpectedEof,
                            ))));
                        }
                        Poll::Pending => {
                            this.inner = SendRecvState::Receiving { framed };
                            return Poll::Pending;
                        }
                    };
                    this.inner = SendRecvState::Closing { framed };

                    return Poll::Ready(Some(Ok(response)));
                }
                SendRecvState::Closing { mut framed } => {
                    match framed.poll_close_unpin(cx) {
                        Poll::Ready(Ok(())) => {
                            this.inner = SendRecvState::Done;
                            continue;
                        }
                        Poll::Ready(Err(e)) => {
                            this.inner = SendRecvState::Done;
                            return Poll::Ready(Some(Err(into_io_error(Error::Recv(e)))));
                        }
                        Poll::Pending => {
                            this.inner = SendRecvState::Closing { framed };
                            return Poll::Pending;
                        }
                    };
                }
                SendRecvState::Done => {
                    return Poll::Ready(None);
                }
                SendRecvState::Poisoned => {
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

impl<S, C, Req, Res, E> Stream for SendRecv<S, C, ReturnStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Item = Req, Error = E> + Decoder<Item = Res, Error = E>,
    E: std::error::Error + Send + Sync + 'static,
    Req: Unpin,
{
    type Item = Result<(Res, S), io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            match mem::replace(&mut this.inner, SendRecvState::Poisoned) {
                SendRecvState::Sending(mut sending) => match sending.poll(cx) {
                    Poll::Ready(Ok(())) => {
                        this.inner = SendRecvState::Flushing {
                            framed: sending.framed,
                        };
                    }
                    Poll::Ready(Err(e)) => {
                        this.inner = SendRecvState::Done;
                        return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
                    }
                    Poll::Pending => {
                        this.inner = SendRecvState::Sending(sending);
                        return Poll::Pending;
                    }
                },
                SendRecvState::Flushing { mut framed } => match framed.poll_flush_unpin(cx) {
                    Poll::Ready(Ok(())) => {
                        this.inner = SendRecvState::Receiving { framed };
                    }
                    Poll::Ready(Err(e)) => {
                        this.inner = SendRecvState::Done;
                        return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
                    }
                    Poll::Pending => {
                        this.inner = SendRecvState::Flushing { framed };
                        return Poll::Pending;
                    }
                },
                SendRecvState::Receiving { mut framed } => {
                    let response = match framed.poll_next_unpin(cx) {
                        Poll::Ready(Some(Ok(request))) => request,
                        Poll::Ready(Some(Err(e))) => {
                            this.inner = SendRecvState::Done;
                            return Poll::Ready(Some(Err(into_io_error(Error::Recv(e)))));
                        }
                        Poll::Ready(None) => {
                            this.inner = SendRecvState::Done;
                            return Poll::Ready(Some(Err(io::Error::from(
                                io::ErrorKind::UnexpectedEof,
                            ))));
                        }
                        Poll::Pending => {
                            this.inner = SendRecvState::Receiving { framed };
                            return Poll::Pending;
                        }
                    };

                    this.inner = SendRecvState::Done;
                    return Poll::Ready(Some(Ok((response, framed.into_parts().io))));
                }
                SendRecvState::Closing { .. } => {
                    unreachable!("We never go into `Closing`")
                }
                SendRecvState::Done => {
                    this.inner = SendRecvState::Done;
                    return Poll::Ready(None);
                }
                SendRecvState::Poisoned => {
                    unreachable!()
                }
            }
        }
    }
}
