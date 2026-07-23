//! Protocol packet codec.
//!
//! Each protocol module exposes pure `build` (encode) and, where the virtual
//! sink needs it, `parse` (decode) functions. No sockets, no threads, no
//! allocation in the encode hot path (callers reuse a buffer). This is the
//! single layer that knows wire formats, shared between the output stage and
//! the virtual sink, and the layer the packet-level tests target (CLAUDE.md §7).

pub mod artnet;
