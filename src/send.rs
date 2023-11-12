use crate::message_patterns::{Error, Sending};
use crate::{CloseStream, Decoder, Encoder, Framed, ReturnStream};
use futures_util::io::{AsyncRead, AsyncWrite};
use futures_util::{SinkExt, Stream};
use std::marker::PhantomData;
use std::{
    io, mem,
    pin::Pin,
    task::{Context, Poll},
};

/// The send message pattern.
///
/// This struct implements a fire-and-forget message pattern for the sending side.
///
/// This struct implements [`Stream`] but each instance will only ever emit one item. The reason
/// this implements [`Stream`] instead of [`Future`] is that a [`Future`] will not be polled after
/// it has resolved. This struct however needs to do more work after sending the message: Close the stream.
///
/// This component works really well when used together with [`SelectAll`](futures::stream::SelectAll).
pub struct Send<S, C, B>
where
    C: Encoder,
{
    inner: SendState<S, C>,
    behaviour: PhantomData<B>,
}

impl<S, C> Send<S, C, ReturnStream>
where
    C: Encoder + Decoder,
    S: AsyncRead + AsyncWrite,
{
    /// TODO
    pub fn new(stream: S, codec: C, message: <C as Encoder>::Item) -> Self {
        Self {
            inner: SendState::Sending(Sending {
                framed: Framed::new(stream, codec),
                message: Some(message),
            }),
            behaviour: Default::default(),
        }
    }

    /// Reconfigure this future to close the stream after the message has been sent instead of returning it.
    pub fn close_after_send(self) -> Send<S, C, CloseStream> {
        Send {
            inner: self.inner,
            behaviour: Default::default(),
        }
    }
}

enum SendState<S, C: Encoder> {
    Sending(Sending<S, C>),
    Flushing { framed: Framed<S, C> },
    Closing { framed: Framed<S, C> },
    Done,
    Poisoned,
}

impl<S, C, Req, Res, E> Stream for Send<S, C, CloseStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Item = Req, Error = E> + Decoder<Item = Res, Error = E>,
    E: From<io::Error> + std::error::Error + std::marker::Send + Sync + 'static,
    Req: Unpin,
{
    type Item = Result<(), io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            match mem::replace(&mut this.inner, SendState::Poisoned) {
                SendState::Sending(mut sending) => match sending.poll(cx) {
                    Poll::Ready(Ok(())) => {
                        this.inner = SendState::Flushing {
                            framed: sending.framed,
                        };
                    }
                    Poll::Ready(Err(e)) => {
                        this.inner = SendState::Done;
                        return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
                    }
                    Poll::Pending => {
                        this.inner = SendState::Sending(sending);
                        return Poll::Pending;
                    }
                },
                SendState::Flushing { mut framed } => match framed.poll_flush_unpin(cx) {
                    Poll::Ready(Ok(())) => {
                        this.inner = SendState::Closing { framed };
                    }
                    Poll::Ready(Err(e)) => {
                        this.inner = SendState::Done;
                        return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
                    }
                    Poll::Pending => {
                        this.inner = SendState::Flushing { framed };
                        return Poll::Pending;
                    }
                },
                SendState::Closing { mut framed } => {
                    match framed.poll_close_unpin(cx) {
                        Poll::Ready(Ok(())) => {
                            this.inner = SendState::Done;
                            continue;
                        }
                        Poll::Ready(Err(e)) => {
                            this.inner = SendState::Done;
                            return Poll::Ready(Some(Err(into_io_error(Error::Recv(e)))));
                        }
                        Poll::Pending => {
                            this.inner = SendState::Closing { framed };
                            return Poll::Pending;
                        }
                    };
                }
                SendState::Done => {
                    return Poll::Ready(None);
                }
                SendState::Poisoned => {
                    unreachable!()
                }
            }
        }
    }
}

fn into_io_error<E>(e: Error<E>) -> io::Error
where
    E: std::error::Error + std::marker::Send + Sync + 'static,
{
    io::Error::new(io::ErrorKind::Other, e)
}

impl<S, C, Req, Res, E> Stream for Send<S, C, ReturnStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Item = Req, Error = E> + Decoder<Item = Res, Error = E>,
    E: std::error::Error + std::marker::Send + Sync + 'static,
    Req: Unpin,
{
    type Item = Result<S, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            match mem::replace(&mut this.inner, SendState::Poisoned) {
                SendState::Sending(mut sending) => match sending.poll(cx) {
                    Poll::Ready(Ok(())) => {
                        this.inner = SendState::Flushing {
                            framed: sending.framed,
                        };
                    }
                    Poll::Ready(Err(e)) => {
                        this.inner = SendState::Done;
                        return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
                    }
                    Poll::Pending => {
                        this.inner = SendState::Sending(sending);
                        return Poll::Pending;
                    }
                },
                SendState::Flushing { mut framed } => match framed.poll_flush_unpin(cx) {
                    Poll::Ready(Ok(())) => {
                        this.inner = SendState::Done;
                        return Poll::Ready(Some(Ok(framed.into_parts().io)));
                    }
                    Poll::Ready(Err(e)) => {
                        this.inner = SendState::Done;
                        return Poll::Ready(Some(Err(into_io_error(Error::Send(e)))));
                    }
                    Poll::Pending => {
                        this.inner = SendState::Flushing { framed };
                        return Poll::Pending;
                    }
                },
                SendState::Closing { .. } => {
                    unreachable!("We never go into `Closing`")
                }
                SendState::Done => {
                    this.inner = SendState::Done;
                    return Poll::Ready(None);
                }
                SendState::Poisoned => {
                    unreachable!()
                }
            }
        }
    }
}
