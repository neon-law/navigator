use super::*;

#[tokio::test]
async fn transition_form_accepts_the_rendered_forward_action_fields() {
    use axum::extract::FromRequest;
    for state in ["in_progress", "done"] {
        let request = axum::http::Request::builder()
            .method(axum::http::Method::POST)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(format!(
                "_csrf=synthetic&next_state={state}"
            )))
            .unwrap();
        let Form(form) = Form::<TaskTransitionForm>::from_request(request, &())
            .await
            .unwrap();
        assert_eq!(form.next_state, state);
    }
}

#[test]
fn only_an_explicitly_empty_assignee_clears_an_assignment() {
    assert_eq!(parse_assignee(" ").unwrap(), None);
    assert_eq!(
        parse_assignee(&Uuid::nil().to_string()).unwrap(),
        Some(Uuid::nil())
    );
    assert!(parse_assignee("not-a-person-id").is_err());
}

#[test]
fn invalid_task_fields_explain_how_to_correct_the_input() {
    for (field, limit) in [("title", "300 bytes"), ("body", "2000 bytes")] {
        let message = flash_for(&store::tasks::ProjectTaskError::Invalid(field));
        assert!(message.contains(limit), "{message}");
        assert!(!message.contains("unavailable"), "{message}");
    }
}
