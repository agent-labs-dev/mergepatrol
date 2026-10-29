# Security

Please report vulnerabilities privately, through [GitHub's private vulnerability reporting](https://github.com/agent-labs-dev/mergepatrol/security/advisories/new), not in a public issue or PR.

Include what an attacker can do, and the steps to reproduce it. We will reply within a few days.

The parts that matter most are the ones that keep a job's untrusted code away from the GitHub App key: the job sandbox, the job protocol, and the checks the orchestrator runs on a job's result. The README's "How jobs are isolated" section describes them. On the hosted deploy, a job's link is the only key to its spec, so who can read it matters too: the README's "On GCP" section says who.
