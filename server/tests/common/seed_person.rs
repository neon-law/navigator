pub async fn seed_person(s: &store::surreal::SurrealDb, email: &str, role: store::persons::Role) {
    store::persons::create(
        s,
        &store::persons::NewPerson::with_role(email.to_string(), email.to_string(), role),
    )
    .await
    .expect("seed person");
}
