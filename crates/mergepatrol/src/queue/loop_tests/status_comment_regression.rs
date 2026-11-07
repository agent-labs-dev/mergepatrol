//! A stack's queue entry can survive individual layers joining or leaving.

use super::*;

#[tokio::test(start_paused = true)]
async fn a_layer_merged_outside_the_queue_gets_a_final_comment_while_its_stack_stays() {
    let run = Run::new(Fake::new(|s| {
        s.add_stack(10, &[1, 2], &[1, 2]);
        s.prs.get_mut(&1).unwrap().is_draft = true;
    }));
    run.tick().await;
    assert!(comments_on(&run, 2).last().unwrap().starts_with("On hold:"));

    // The stack endpoint returns only open layers, as GitHub::stack_layers does.
    run.gh.with_mut(|s| {
        s.prs.get_mut(&2).unwrap().state = "MERGED".into();
        s.merged.push(2);
        s.merge_commits.insert(2, "outside-sha".into());
    });
    run.tick().await;
    assert_eq!(
        run.lane(),
        [(10, "Held(Layer { number: 1, hold: Draft })".to_owned())]
    );
    assert_eq!(
        comments_on(&run, 2).last().unwrap(),
        "Merged outside MergeLoop as outside-sha."
    );
}

#[tokio::test(start_paused = true)]
async fn a_new_layer_gets_a_status_comment_when_it_joins_an_already_held_stack() {
    let run = Run::new(Fake::new(|s| {
        s.add_stack(10, &[1, 2], &[1]);
        s.prs.get_mut(&1).unwrap().is_draft = true;
    }));
    run.tick().await;
    run.gh.with_mut(|s| {
        s.add_pr(3);
        let pr = s.prs.get_mut(&3).unwrap();
        pr.labels.clear();
        pr.base_ref_name = "pr-2".into();
        pr.stack = Some(StackRef {
            number: 10,
            base_ref_name: "main".into(),
        });
        s.stacks.get_mut(&10).unwrap().push(3);
    });
    run.tick().await;
    assert_eq!(
        run.lane(),
        [(10, "Held(Layer { number: 1, hold: Draft })".to_owned())]
    );
    assert!(
        !comments_on(&run, 3).is_empty(),
        "the new layer is queued but has no status comment"
    );
}
