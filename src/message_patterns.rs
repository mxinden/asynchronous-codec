use crate::{recvsend, Encoder, Framed};
use futures_util::{AsyncRead, AsyncWrite, SinkExt};
use std::task::{Context, Poll};
use std::{fmt, io};

/// Marker type for a [`SendingResponse`] future that will close the stream after the message has been sent.
pub enum CloseStream {}

/// Marker type for a [`SendingResponse`] future that will return the stream back to the user after the message has been sent.
///
/// This may be useful if multiple request-response exchanges should happen on the same stream.
pub enum ReturnStream {}

#[derive(Debug)]
pub enum Error<Enc> {
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

pub struct Sending<S, C>
where
    C: Encoder,
{
    pub(crate) framed: Framed<S, C>,
    pub(crate) message: Option<C::Item>,
}

impl<S, C, E> Sending<S, C>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Error = E>,
    E: std::error::Error + Send + Sync + 'static,
{
    pub fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        futures_util::ready!(self.framed.poll_ready_unpin(cx).map_err(Error::Send))
            .map_err(recvsend::into_io_error)?;

        self.framed
            .start_send_unpin(
                self.message
                    .take()
                    .expect("to not be polled after completion"),
            )
            .map_err(Error::Send)
            .map_err(recvsend::into_io_error)?;

        Poll::Ready(Ok(()))
    }
}
