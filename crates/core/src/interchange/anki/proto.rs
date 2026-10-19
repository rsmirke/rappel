//! The few protobuf messages an Anki package has, decoded by hand
//! (ADR 0061): the package version in `meta`, the media list in `media`,
//! and in a current collection the kind of a note type and the formats of
//! a card template. Only varints and length-delimited fields are read;
//! anything malformed is an error, never a panic, and nothing is
//! allocated but the values asked for.

/// A field's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    /// A fixed 32- or 64-bit value, not used by the messages read.
    Fixed,
}

/// The message is not valid protobuf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Malformed;

/// The fields of one message, in order.
#[derive(Debug, Clone)]
pub(super) struct Fields<'a> {
    rest: &'a [u8],
}

impl<'a> Fields<'a> {
    pub(super) fn new(message: &'a [u8]) -> Self {
        Self { rest: message }
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, Value<'a>), Malformed>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let item = self.field();
        if item.is_err() {
            // Nothing more after an error.
            self.rest = &[];
        }
        Some(item)
    }
}

impl<'a> Fields<'a> {
    fn field(&mut self) -> Result<(u64, Value<'a>), Malformed> {
        let key = self.varint()?;
        let number = key >> 3;
        if number == 0 {
            return Err(Malformed);
        }
        let value = match key & 7 {
            0 => Value::Varint(self.varint()?),
            1 => {
                self.skip(8)?;
                Value::Fixed
            }
            2 => {
                let len = usize::try_from(self.varint()?).map_err(|_| Malformed)?;
                let (bytes, rest) = self.rest.split_at_checked(len).ok_or(Malformed)?;
                self.rest = rest;
                Value::Bytes(bytes)
            }
            5 => {
                self.skip(4)?;
                Value::Fixed
            }
            // Groups (3, 4) are not used by these messages.
            _ => return Err(Malformed),
        };
        Ok((number, value))
    }

    fn skip(&mut self, n: usize) -> Result<(), Malformed> {
        self.rest = self.rest.get(n..).ok_or(Malformed)?;
        Ok(())
    }

    fn varint(&mut self) -> Result<u64, Malformed> {
        let mut value = 0_u64;
        for (i, byte) in self.rest.iter().enumerate().take(10) {
            let bits = u64::from(byte & 0x7f);
            // The tenth byte may only hold the top bit.
            if i == 9 && bits > 1 {
                return Err(Malformed);
            }
            value |= bits << (7 * i);
            if byte & 0x80 == 0 {
                self.rest = &self.rest[i + 1..];
                return Ok(value);
            }
        }
        Err(Malformed)
    }
}

/// The last varint of field `number` in `message` (protobuf keeps the last
/// one of a repeated scalar), or `None` if it has none.
pub(super) fn varint_field(message: &[u8], number: u64) -> Result<Option<u64>, Malformed> {
    let mut found = None;
    for field in Fields::new(message) {
        if let (n, Value::Varint(v)) = field?
            && n == number
        {
            found = Some(v);
        }
    }
    Ok(found)
}

/// The last length-delimited field `number` of `message` as UTF-8 text,
/// or `None` if it has none.
pub(super) fn text_field(message: &[u8], number: u64) -> Result<Option<&str>, Malformed> {
    let mut found = None;
    for field in Fields::new(message) {
        if let (n, Value::Bytes(b)) = field?
            && n == number
        {
            found = Some(std::str::from_utf8(b).map_err(|_| Malformed)?);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_read_in_order() {
        // 1: varint 3; 2: "ab"; 3: fixed64; 4: fixed32; 5: varint 300.
        let message = [
            0x08, 3, 0x12, 2, b'a', b'b', 0x19, 0, 0, 0, 0, 0, 0, 0, 0, 0x25, 0, 0, 0, 0, 0x28,
            0xac, 0x02,
        ];
        let fields: Vec<_> = Fields::new(&message).collect::<Result<_, _>>().unwrap();
        assert_eq!(
            fields,
            [
                (1, Value::Varint(3)),
                (2, Value::Bytes(b"ab")),
                (3, Value::Fixed),
                (4, Value::Fixed),
                (5, Value::Varint(300)),
            ]
        );
        assert_eq!(varint_field(&message, 5), Ok(Some(300)));
        assert_eq!(text_field(&message, 2), Ok(Some("ab")));
        assert_eq!(varint_field(&message, 9), Ok(None));
    }

    #[test]
    fn malformed_messages_are_errors() {
        for message in [
            &[0x12, 5, b'a'][..], // shorter than its length
            &[0x08][..],          // a key without its value
            &[0x08, 0x80][..],    // a varint cut short
            &[0x0b][..],          // a group
            &[0x00, 1][..],       // field number 0
            &[
                0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f,
            ][..], // > 64 bits
            &[0x12, 0xff, 0xff, 0xff, 0xff, 0x0f][..], // a huge length
        ] {
            assert!(
                Fields::new(message).any(|f| f.is_err()),
                "{message:?} should be refused"
            );
        }
        assert_eq!(text_field(&[0x12, 1, 0xff], 2), Err(Malformed));
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        for _ in 0..2_000 {
            let len = usize::try_from(state % 64).unwrap();
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state.to_le_bytes()[0]
                })
                .collect();
            let _ = Fields::new(&bytes).count();
            let _ = varint_field(&bytes, 1);
            let _ = text_field(&bytes, 2);
        }
    }
}
