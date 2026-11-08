use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct WebFetch {
    /// The URL that was fetched.
    pub url: String,
    /// The page's title, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The byline the page declares, when it declares one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byline: Option<String>,
    /// The content type the server returned, which decides how the body was
    /// reduced to text.
    pub content_type: String,
    /// The readable text of the page.
    pub content: String,
    /// Whether the body cap dropped content.
    pub truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchResult {
    /// The result's title.
    pub title: String,
    /// The result's URL.
    pub url: String,
    /// The excerpt the search provider returned.
    pub content: String,
    /// When the page was published, when the provider says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_date: Option<String>,
    /// Who wrote it, when the provider says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct WebSearch {
    /// The query that was searched.
    pub query: String,
    /// The results, best first.
    pub results: Vec<SearchResult>,
}
