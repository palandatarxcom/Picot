// ABOUTME: Loads canonical runtime mutation command names shared with browser validation.
// ABOUTME: Keeps Rust admission checks data-driven while preserving fail-closed unknown commands.

use std::collections::HashSet;
use std::sync::OnceLock;

#[allow(dead_code)]
static MUTATION_TYPES: OnceLock<HashSet<&'static str>> = OnceLock::new();

#[allow(dead_code)]
pub fn is_mutation(command_type: &str) -> bool {
    MUTATION_TYPES
        .get_or_init(|| {
            serde_json::from_str(include_str!("../../../../shared/mutation-types.json"))
                .expect("shared mutation types must be valid JSON")
        })
        .contains(command_type)
}

#[cfg(test)]
mod tests {
    use super::is_mutation;

    #[test]
    fn recognizes_canonical_mutations_and_rejects_unknowns() {
        assert!(is_mutation("prompt"));
        assert!(is_mutation("set_follow_up_mode"));
        assert!(!is_mutation("get_state"));
        assert!(!is_mutation("unknown"));
    }
}
