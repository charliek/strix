/// Normalise a name for case-insensitive matching: trimmed, lowercased, with
/// `_` treated as `-` (so `tokyo_night` and `tokyo-night` are the same).
pub fn normalize(name: &str) -> String {
    name.trim().to_lowercase().replace('_', "-")
}
