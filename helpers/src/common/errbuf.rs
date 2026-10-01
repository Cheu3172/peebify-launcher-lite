// ------------ Error Buffer ------------
// A fixed-size error message that lives in shared memory. One side writes it, the other reads it back, and long
// text is cut on a character boundary so it never ends mid-letter.

use core::sync::atomic::{AtomicU8, Ordering};

pub fn clear(buffer: &[AtomicU8]) {
    for byte in buffer {
        byte.store(0, Ordering::Relaxed);
    }
}

pub fn write(buffer: &[AtomicU8], message: &str) {
    let bytes = message.as_bytes();
    let mut len = bytes.len().min(buffer.len().saturating_sub(1));
    while !message.is_char_boundary(len) {
        len -= 1;
    }
    for (slot, byte) in buffer.iter().zip(&bytes[..len]) {
        slot.store(*byte, Ordering::Relaxed);
    }
    for slot in buffer[len..].iter() {
        slot.store(0, Ordering::Relaxed);
    }
}

pub fn read(buffer: &[AtomicU8]) -> String {
    let bytes: Vec<u8> = buffer
        .iter()
        .map(|b| b.load(Ordering::Relaxed))
        .take_while(|b| *b != 0)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(len: usize) -> Vec<AtomicU8> {
        (0..len).map(|_| AtomicU8::new(0xFF)).collect()
    }

    #[test]
    fn round_trips_a_short_message() {
        let buffer = buffer(16);
        write(&buffer, "no game");
        assert_eq!(read(&buffer), "no game");
    }

    #[test]
    fn truncation_keeps_whole_characters() {
        let buffer = buffer(8);
        write(&buffer, "abcdefé");
        assert_eq!(read(&buffer), "abcdef");
        write(&buffer, "ab\u{1F600}cdef");
        assert_eq!(read(&buffer), "ab\u{1F600}c");
        write(&buffer, "\u{1F600}\u{1F600}");
        assert_eq!(read(&buffer), "\u{1F600}");
    }

    #[test]
    fn shorter_message_clears_the_old_tail() {
        let buffer = buffer(8);
        write(&buffer, "abcdefg");
        write(&buffer, "xy");
        assert_eq!(read(&buffer), "xy");
    }
}
