//! A verifier: one fresh agent that checks one finding of a review on its own, given only what
//! the finding claims and the code. It never sees how the reviewer got there, nor what other
//! verifiers said. A panel of them votes on whether the finding holds.

use mergepatrol_core::{Finding, Verdict, Vote};

use crate::{VerifyRequest, last_line};

pub const HOLDS: &str = "VERDICT: holds";
pub const DOES_NOT_HOLD: &str = "VERDICT: does not hold";

/// What a finding claims, as a verifier reads it: what and where, what it saw, and the change it
/// asks for. Not its confidence, what it checked, what it was unsure of or what else it read:
/// that is how the reviewer got there, and the verifier finds its own way.
pub fn claim(f: &Finding) -> String {
    let place = crate::where_is(f);
    format!(
        "kind: {}\ntitle: {}\nwhere: {place}\nfinding: {}\nsuggestion: {}",
        f.kind.as_str(),
        f.title.trim(),
        f.finding.trim(),
        f.suggestion.trim(),
    )
}

pub(crate) fn verify_prompt(req: &VerifyRequest<'_>) -> String {
    let (title, base, claim) = (req.pr_title, req.base, req.claim);
    format!(
        "You are checking one finding of a code review of a pull request, {title}, before a merge \
queue merges it. You did not write the pull request or the review, and you do not see how the \
reviewer got there: only what the finding claims.

The claim:
{claim}

The pull request's changes are `git diff {base}..HEAD`. Read the code the claim is about and what \
it rests on: the callers, how input reaches it, what checks or guards it, the config it runs \
under. Use `git log`, `git show` and `git diff`, and read files. Change nothing.

Hold the claim to the code, not to how sure it sounds. It holds only when the code as it is lets \
what it describes happen: you can name the path. It does not hold when something the claim missed \
stops it, such as a check, a caller that never passes such input, or a setting. When the code \
cannot tell, it does not hold.

Answer in a short paragraph of plain text: what you found. Then end with one line, alone: \
`{HOLDS}` or `{DOES_NOT_HOLD}`."
    )
}

/// The vote in a verifier's answer: its last line, with the rest as why. An answer with no
/// verdict abstains.
pub fn vote(answer: &str) -> Vote {
    let verdict = match last_line(answer) {
        Some(l) if l.eq_ignore_ascii_case(HOLDS) => Verdict::Holds,
        Some(l) if l.eq_ignore_ascii_case(DOES_NOT_HOLD) => Verdict::DoesNotHold,
        _ => {
            return Vote {
                verdict: Verdict::Abstained,
                why: answer.trim().to_owned(),
            };
        }
    };
    let why = answer
        .trim_end()
        .rsplit_once('\n')
        .map_or("", |(why, _)| why);
    Vote {
        verdict,
        why: why.trim().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use mergepatrol_core::{Kind, Lean};

    use super::*;

    fn finding() -> Finding {
        Finding {
            id: "f2".into(),
            kind: Kind::Security,
            title: "The token check compares with ==".into(),
            file: Some("src/auth.rs".into()),
            line: Some(41),
            confidence: 70,
            lean: Lean::Change,
            finding: "A timing attack leaks the token.".into(),
            unsure: "Whether the proxy rate-limits.".into(),
            checked: vec!["The caller passes the raw header".into()],
            would_clear: "The proxy's config".into(),
            suggestion: "Compare in constant time.".into(),
            evidence: None,
            needs_human: false,
            settled: None,
            verified: None,
            code: Vec::new(),
            related: Vec::new(),
            also_at: Vec::new(),
        }
    }

    #[test]
    fn a_verifier_sees_what_a_finding_claims_not_how_the_reviewer_got_there() {
        let c = claim(&finding());
        for said in [
            "kind: security",
            "The token check compares with ==",
            "where: src/auth.rs:41",
            "A timing attack leaks the token.",
            "Compare in constant time.",
        ] {
            assert!(c.contains(said), "{said}");
        }
        for reasoning in ["rate-limits", "raw header", "proxy's config", "70"] {
            assert!(!c.contains(reasoning), "{reasoning}");
        }
        let p = verify_prompt(&VerifyRequest {
            workdir: Path::new("."),
            base: "abc",
            pr_title: "t",
            claim: &c,
            transcript: Path::new("t"),
        });
        assert!(p.contains(&c));
        assert!(p.contains("git diff abc..HEAD"));
        assert!(p.contains("When the code cannot tell, it does not hold."));
        assert!(p.ends_with(&format!("`{HOLDS}` or `{DOES_NOT_HOLD}`.")));
    }

    #[test]
    fn a_verifier_checks_a_recurring_finding_at_every_place() {
        let mut f = finding();
        f.also_at = vec![mergepatrol_core::Place {
            file: "src/admin.rs".into(),
            line: Some(7),
            code: Vec::new(),
        }];
        assert!(claim(&f).contains("where: src/auth.rs:41, src/admin.rs:7\n"));
        f.file = None;
        f.also_at.clear();
        assert!(claim(&f).contains("where: the whole PR\n"));
    }

    #[test]
    fn a_vote_is_its_last_line_and_anything_else_abstains() {
        let v = vote("The header reaches it unchecked.\n\nVERDICT: holds\n");
        assert_eq!(v.verdict, Verdict::Holds);
        assert_eq!(v.why, "The header reaches it unchecked.");
        let v = vote("A middleware rejects it first.\n`verdict: does not hold`");
        assert_eq!(v.verdict, Verdict::DoesNotHold);
        assert_eq!(v.why, "A middleware rejects it first.");
        let v = vote("VERDICT: holds\nBut on second look a middleware stops it.");
        assert_eq!(v.verdict, Verdict::Abstained);
        assert!(v.why.contains("second look"));
        assert_eq!(vote("").verdict, Verdict::Abstained);
        // However the agent dresses the line.
        assert_eq!(
            vote("Reached.\n**VERDICT: holds.**").verdict,
            Verdict::Holds
        );
    }
}
