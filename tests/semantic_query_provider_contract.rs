use eal_api::query_embedding::QueryEmbeddingService;
use eal_query::analyze_query;
use eal_semantic_contracts::EmbeddingInputKind;

#[test]
fn natural_language_queries_preserve_complete_text_and_companion_views() {
    let query = "Notify me when Acme Corporation launches renewable energy tools in Colombia.";
    let views = analyze_query(query).expect("query should be valid");

    assert_eq!(views.embedding_inputs[0].kind, EmbeddingInputKind::Query);
    assert_eq!(views.embedding_inputs[0].text, query);
    assert!(
        views
            .embedding_inputs
            .iter()
            .any(|input| input.kind == EmbeddingInputKind::Keyword)
    );
    assert!(
        views
            .embedding_inputs
            .iter()
            .any(|input| input.kind == EmbeddingInputKind::Entity)
    );
}

#[test]
fn provider_module_is_compiled_by_the_api_test_suite() {
    let type_name = std::any::type_name::<QueryEmbeddingService>();
    assert!(type_name.ends_with("QueryEmbeddingService"));
}
