//! Closest-match ranking for unresolved queries.

/// Levenshtein distance between two normalized names, over characters, with
/// a single rolling row — O(len_a × len_b) time, O(len_b) space.
#[must_use]
pub(crate) fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();

    for (i, char_a) in a.chars().enumerate() {
        let mut current: Vec<usize> = Vec::with_capacity(b.len() + 1);
        current.push(i + 1);
        for (j, char_b) in b.iter().enumerate() {
            let deletion = previous[j + 1] + 1;
            let insertion = current[j] + 1;
            let substitution = previous[j] + usize::from(char_a != *char_b);
            current.push(deletion.min(insertion).min(substitution));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_match_reference_values() {
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("springfield", "springfield"), 0);
        assert_eq!(distance("sprinfield", "springfield"), 1);
        assert_eq!(distance("abc", ""), 3);
        assert_eq!(distance("", "abc"), 3);
    }
}
