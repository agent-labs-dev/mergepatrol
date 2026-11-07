//! The skill MergeLoop installs tells agents how a reply to a finding ends: every verdict the
//! reviewer may give.

use mergepatrol_resolver::{INTENDED, STANDS, WITHDRAWN};

const SKILL: &str = include_str!("../src/skill.md");

#[test]
fn the_skill_names_every_verdict_on_a_reply() {
    for verdict in [WITHDRAWN, INTENDED, STANDS] {
        assert!(SKILL.contains(verdict), "skill.md never names `{verdict}`");
    }
}
