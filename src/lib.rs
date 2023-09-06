#![deny(missing_docs)]
//! Utilities for encoding and decoding frames using `async/await`.
//!
//! Contains adapters to go from streams of bytes, [`AsyncRead`](futures::io::AsyncRead)
//! and [`AsyncWrite`](futures::io::AsyncWrite), to framed streams implementing [`Sink`](futures::Sink) and [`Stream`](futures::Stream).
//! Framed streams are also known as `transports`.
//!
//! ```
//! futures::executor::block_on(async move {
//! use futures::TryStreamExt;
//! use futures::io::Cursor;
//! use asynchronous_codec::{LinesCodec, Framed, FramedError};
//!
//! let io = Cursor::new(Vec::new());
//! let mut framed = Framed::new(io, LinesCodec);
//!
//! while let Some(line) = framed.try_next().await? {
//!     dbg!(line);
//! }
//! # Ok::<_, FramedError<LinesCodec>>(())
//! # }).unwrap();
//! ```

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub use bytes::{Bytes, BytesMut};

mod codec;

#[cfg(feature = "std")]
pub use codec::{BytesCodec, LengthCodec, LinesCodec};

#[cfg(feature = "cbor")]
pub use codec::{CborCodec, CborCodecError};
#[cfg(feature = "json")]
pub use codec::{JsonCodec, JsonCodecError};

mod decoder;
pub use decoder::Decoder;

mod encoder;
pub use encoder::Encoder;

#[cfg(feature = "std")]
mod framed;

#[cfg(feature = "std")]
pub use framed::{Framed, FramedParts, FramedError};


#[cfg(feature = "std")]
mod framed_read;

#[cfg(feature = "std")]
pub use framed_read::{FramedRead, FramedReadParts, FramedReadError};

#[cfg(feature = "std")]
mod framed_write;

#[cfg(feature = "std")]
pub use framed_write::{FramedWrite, FramedWriteParts, FramedWriteError};

#[cfg(feature = "std")]
mod fuse;
