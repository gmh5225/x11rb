//! Helper for implementing `RequestConnection::extension_information()`.

use std::collections::{hash_map::Entry as HashMapEntry, HashMap};

use crate::connection::{RequestConnection, SequenceNumber};
use crate::cookie::Cookie;
use crate::errors::{ConnectionError, ReplyError};
use crate::xproto::{ConnectionExt, QueryExtensionReply};

/// Helper for implementing `RequestConnection::extension_information()`.
///
/// This helps with implementing `RequestConnection`. Most likely, you do not need this in your own
/// code, unless you really want to implement your own X11 connection.
#[derive(Debug, Default)]
pub struct ExtensionInformation(HashMap<&'static str, CheckState>);

#[derive(Debug)]
enum CheckState {
    Prefetched(SequenceNumber),
    Present(QueryExtensionReply),
    Missing,
    Error,
}

impl ExtensionInformation {
    /// If the extension has not prefetched yet, sends a `QueryExtension`
    /// requests, adds a field to the hash map and returns a reference to it.
    fn prefetch_extension_information_aux<C: RequestConnection>(
        &mut self,
        conn: &C,
        extension_name: &'static str,
    ) -> Result<&mut CheckState, ConnectionError> {
        match self.0.entry(extension_name) {
            // Extension already checked, return the cached value
            HashMapEntry::Occupied(entry) => Ok(entry.into_mut()),
            HashMapEntry::Vacant(entry) => {
                let cookie = conn.query_extension(extension_name.as_bytes())?;
                Ok(entry.insert(CheckState::Prefetched(cookie.into_sequence_number())))
            }
        }
    }

    /// Prefetchs an extension sending a `QueryExtension` without waiting for
    /// the reply.
    pub fn prefetch_extension_information<C: RequestConnection>(
        &mut self,
        conn: &C,
        extension_name: &'static str,
    ) -> Result<(), ConnectionError> {
        // We are not interested on the reference to the entry.
        let _ = self.prefetch_extension_information_aux(conn, extension_name)?;
        Ok(())
    }

    /// An implementation of `RequestConnection::extension_information()`.
    ///
    /// The given connection is used for sending a `QueryExtension` request if needed.
    pub fn extension_information<C: RequestConnection>(
        &mut self,
        conn: &C,
        extension_name: &'static str,
    ) -> Result<Option<QueryExtensionReply>, ConnectionError> {
        let entry = self.prefetch_extension_information_aux(conn, extension_name)?;
        match entry {
            CheckState::Prefetched(sequence_number) => {
                match Cookie::<C, QueryExtensionReply>::new(conn, *sequence_number).reply() {
                    Err(err) => {
                        *entry = CheckState::Error;
                        match err {
                            ReplyError::ConnectionError(e) => Err(e),
                            // The X11 protocol specification does not specify any error
                            // for the QueryExtension request, so this should not happen.
                            ReplyError::X11Error(_) => Err(ConnectionError::UnknownError),
                        }
                    }
                    Ok(info) => {
                        if info.present {
                            *entry = CheckState::Present(info);
                            Ok(Some(info))
                        } else {
                            *entry = CheckState::Missing;
                            Ok(None)
                        }
                    }
                }
            }
            CheckState::Present(info) => Ok(Some(*info)),
            CheckState::Missing => Ok(None),
            CheckState::Error => Err(ConnectionError::UnknownError),
        }
    }

    /// Create an iterator that allows to iterate over the known present extensions.
    ///
    /// This function returns an iterator that provides information about all the extensions that
    /// were queried and found to be present. Extensions that were not queried or which are not
    /// present are not returned.
    pub fn known_present<'a>(
        &'a self,
    ) -> impl 'a + Iterator<Item = (&'static str, QueryExtensionReply)> {
        self.0.iter().filter_map(|(name, state)| {
            if let CheckState::Present(reply) = state {
                Some((*name, *reply))
            } else {
                None
            }
        })
    }
}

#[cfg(test)]
mod test {
    use std::cell::RefCell;
    use std::convert::TryFrom;
    use std::io::IoSlice;

    use crate::connection::{
        BufWithFds, DiscardMode, RequestConnection, RequestKind, SequenceNumber,
    };
    use crate::cookie::{Cookie, CookieWithFds, VoidCookie};
    use crate::errors::{ConnectionError, ParseError, ReplyError};
    use crate::utils::RawFdContainer;
    use crate::x11_utils::GenericError;
    use crate::xproto::QueryExtensionReply;

    use super::{CheckState, ExtensionInformation};

    struct FakeConnection(RefCell<SequenceNumber>);

    impl RequestConnection for FakeConnection {
        type Buf = Vec<u8>;

        fn send_request_with_reply<R>(
            &self,
            _bufs: &[IoSlice<'_>],
            _fds: Vec<RawFdContainer>,
        ) -> Result<Cookie<'_, Self, R>, ConnectionError>
        where
            R: for<'a> TryFrom<&'a [u8], Error = ParseError>,
        {
            Ok(Cookie::new(self, 1))
        }

        fn send_request_with_reply_with_fds<R>(
            &self,
            _bufs: &[IoSlice<'_>],
            _fds: Vec<RawFdContainer>,
        ) -> Result<CookieWithFds<'_, Self, R>, ConnectionError>
        where
            R: for<'a> TryFrom<(&'a [u8], Vec<RawFdContainer>), Error = ParseError>,
        {
            unimplemented!()
        }

        fn send_request_without_reply(
            &self,
            _bufs: &[IoSlice<'_>],
            _fds: Vec<RawFdContainer>,
        ) -> Result<VoidCookie<'_, Self>, ConnectionError> {
            unimplemented!()
        }

        fn discard_reply(&self, _sequence: SequenceNumber, _kind: RequestKind, _mode: DiscardMode) {
            unimplemented!()
        }

        fn prefetch_extension_information(
            &self,
            _extension_name: &'static str,
        ) -> Result<(), ConnectionError> {
            unimplemented!();
        }

        fn extension_information(
            &self,
            _extension_name: &'static str,
        ) -> Result<Option<QueryExtensionReply>, ConnectionError> {
            unimplemented!()
        }

        fn wait_for_reply_or_error(
            &self,
            sequence: SequenceNumber,
        ) -> Result<Vec<u8>, ReplyError<Vec<u8>>> {
            // Code should only ask once for the reply to a request. Check that this is the case
            // (by requiring monotonically increasing sequence numbers here).
            let mut last = self.0.borrow_mut();
            assert!(
                *last < sequence,
                "Last sequence number that was awaited was {}, but now {}",
                *last,
                sequence
            );
            *last = sequence;
            // Then return an error, because that's what the #[test] below needs.
            Err(ReplyError::ConnectionError(ConnectionError::UnknownError))
        }

        fn wait_for_reply(
            &self,
            _sequence: SequenceNumber,
        ) -> Result<Option<Vec<u8>>, ConnectionError> {
            unimplemented!()
        }

        fn wait_for_reply_with_fds(
            &self,
            _sequence: SequenceNumber,
        ) -> Result<BufWithFds<Vec<u8>>, ReplyError<Vec<u8>>> {
            unimplemented!()
        }

        fn check_for_error(
            &self,
            _sequence: SequenceNumber,
        ) -> Result<Option<GenericError<Vec<u8>>>, ConnectionError> {
            unimplemented!()
        }

        fn maximum_request_bytes(&self) -> usize {
            0
        }

        fn prefetch_maximum_request_bytes(&self) {
            unimplemented!()
        }
    }

    #[test]
    fn test_double_await() {
        let conn = FakeConnection(RefCell::new(0));
        let mut ext_info = ExtensionInformation::default();

        // Ask for an extension info. FakeConnection will return an error.
        match ext_info.extension_information(&conn, "whatever") {
            Err(ConnectionError::UnknownError) => {}
            r => panic!("Unexpected result: {:?}", r),
        }

        // Ask again for the extension information. ExtensionInformation should not try to get the
        // reply again, because that would just hang. Once upon a time, this caused a hang.
        match ext_info.extension_information(&conn, "whatever") {
            Err(ConnectionError::UnknownError) => {}
            r => panic!("Unexpected result: {:?}", r),
        }
    }

    #[test]
    fn test_iter() {
        let reply = QueryExtensionReply {
            response_type: 1,
            sequence: 2,
            length: 3,
            present: true,
            major_opcode: 4,
            first_event: 5,
            first_error: 6,
        };

        let mut ext_info = ExtensionInformation::default();
        let _ = ext_info.0.insert("prefetched", CheckState::Prefetched(42));
        let _ = ext_info.0.insert("present", CheckState::Present(reply));
        let _ = ext_info.0.insert("missing", CheckState::Missing);
        let _ = ext_info.0.insert("error", CheckState::Error);

        let known = ext_info.known_present().collect::<Vec<_>>();
        assert_eq!(known, [("present", reply)]);
    }
}
