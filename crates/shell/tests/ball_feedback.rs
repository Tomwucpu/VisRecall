use visrecall_shell::ball::{BallFeedback, BallFeedbackHandle};

#[test]
fn feedback_roundtrips_through_the_payload() {
    for feedback in [
        BallFeedback::Busy { progress: None },
        BallFeedback::Busy { progress: Some(0) },
        BallFeedback::Busy { progress: Some(99) },
        BallFeedback::Success,
        BallFeedback::Failure,
    ] {
        assert_eq!(BallFeedback::unpack(feedback.pack()), feedback);
    }
}

#[test]
fn a_detached_handle_is_a_no_op() {
    BallFeedbackHandle::detached().send(BallFeedback::Success);
}
