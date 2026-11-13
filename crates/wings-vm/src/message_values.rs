pub(crate) const MAX_LIST_ELEMENTS: usize = 64;

#[derive(Clone, Copy)]
pub(crate) struct ListPayload {
    pub mask: u64,
    pub count: usize,
    pub values: [i64; MAX_LIST_ELEMENTS],
}

impl Default for ListPayload {
    fn default() -> Self {
        Self {
            mask: 0,
            count: 0,
            values: [0; MAX_LIST_ELEMENTS],
        }
    }
}

impl ListPayload {
    pub(crate) fn valid(&self, arguments: &[i64], expected_mask: u64) -> bool {
        if arguments.len() > 64
            || self.mask != expected_mask
            || self.count > MAX_LIST_ELEMENTS
            || (arguments.len() < 64 && self.mask >> arguments.len() != 0)
        {
            return false;
        }
        let mut cursor = 0;
        for (index, argument) in arguments.iter().enumerate() {
            if self.mask & (1u64 << index) != 0 {
                let (start, length) = Self::segment(*argument);
                if start != cursor || length > self.count - cursor {
                    return false;
                }
                cursor += length;
            }
        }
        cursor == self.count
    }

    pub(crate) fn segment(encoded: i64) -> (usize, usize) {
        (encoded as u32 as usize, ((encoded as u64) >> 32) as usize)
    }

    pub(crate) fn encode(start: usize, length: usize) -> i64 {
        ((length as u64) << 32 | start as u64) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_segments_are_canonical_bounded_and_match_the_schema() {
        let mut payload = ListPayload {
            mask: 5,
            count: 64,
            ..ListPayload::default()
        };
        let arguments = [ListPayload::encode(0, 40), 9, ListPayload::encode(40, 24)];
        assert!(payload.valid(&arguments, 5));
        for mask in [0, 1, 4, 7, 8, u64::MAX] {
            assert!(!payload.valid(&arguments, mask));
        }
        for malformed in [
            [ListPayload::encode(1, 40), 9, ListPayload::encode(40, 24)],
            [ListPayload::encode(0, 40), 9, ListPayload::encode(39, 25)],
            [ListPayload::encode(0, 40), 9, ListPayload::encode(41, 23)],
            [ListPayload::encode(0, 40), 9, ListPayload::encode(40, 25)],
            [ListPayload::encode(0, 40), 9, -1],
        ] {
            assert!(!payload.valid(&malformed, 5));
        }
        payload.count = 65;
        assert!(!payload.valid(&arguments, 5));
        payload.mask = 1 << 63;
        payload.count = 0;
        assert!(payload.valid(&[0; 64], 1 << 63));
        assert!(!payload.valid(&[0; 63], 1 << 63));
        payload.mask = 0;
        assert!(payload.valid(&[], 0));
        payload.count = 1;
        assert!(!payload.valid(&[], 0));
    }
}
