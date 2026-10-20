//! System One request and response bodies.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// `POST {base_url}/systemone`'s body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionRequest {
    pub model: String,
    /// Text (a JSON string) or any JSON value the model reads as the state.
    pub state: serde_json::Value,
    /// 1-64 named questions.
    pub questions: BTreeMap<String, Question>,
}

/// One named question, tagged by `type`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Pick one label; `criteria` maps each label to an optional
    /// description.
    Choice {
        instructions: String,
        criteria: BTreeMap<String, Option<String>>,
    },
    /// A yes/no question answered with the probability of true.
    Noul {
        instructions: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
}

/// Optional descriptions of what `false` and `true` mean for a noul
/// question.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NoulCriteria {
    #[serde(rename = "false", default, skip_serializing_if = "Option::is_none")]
    pub if_false: Option<String>,
    #[serde(rename = "true", default, skip_serializing_if = "Option::is_none")]
    pub if_true: Option<String>,
}

/// The response body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionResponse {
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    #[serde(default)]
    pub usage: Usage,
}

/// One answer, tagged by `type`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    /// `noul` is the probability of true.
    Noul { noul: f64 },
    /// An answer type this client does not ask for (e.g. `score`).
    #[serde(other)]
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    // The example from ollama.com/library/clef, verbatim.
    const EXAMPLE_REQUEST: &str = r#"{
  "model": "clef:cloud",
  "state": {"ticket": "I was charged twice. Please refund the extra payment."},
  "questions": {
    "team": {
      "type": "choice",
      "instructions": "Which team should handle this ticket?",
      "criteria": {
        "billing": "Payments and refunds",
        "technical": "Bugs and integrations",
        "other": "None of the above"
      }
    },
    "refund": {
      "type": "noul",
      "instructions": "Does the customer explicitly ask for a refund?"
    }
  }
}"#;

    const EXAMPLE_RESPONSE: &str = r#"{
  "model": "clef",
  "answers": {
    "team": {
      "type": "choice",
      "choice": "billing",
      "probabilities": {"billing": 0.9888, "technical": 0.0044, "other": 0.0068},
      "confidence": 0.9667
    },
    "refund": {"type": "noul", "noul": 0.9916}
  },
  "usage": {"input_tokens": 332, "output_tokens": 0}
}"#;

    fn example_request() -> DecisionRequest {
        let criteria = [
            ("billing", "Payments and refunds"),
            ("technical", "Bugs and integrations"),
            ("other", "None of the above"),
        ]
        .into_iter()
        .map(|(label, description)| (label.to_string(), Some(description.to_string())))
        .collect();
        DecisionRequest {
            model: "clef:cloud".into(),
            state: serde_json::json!({
                "ticket": "I was charged twice. Please refund the extra payment."
            }),
            questions: BTreeMap::from([
                (
                    "team".to_string(),
                    Question::Choice {
                        instructions: "Which team should handle this ticket?".into(),
                        criteria,
                    },
                ),
                (
                    "refund".to_string(),
                    Question::Noul {
                        instructions: "Does the customer explicitly ask for a refund?".into(),
                        criteria: None,
                    },
                ),
            ]),
        }
    }

    #[test]
    fn the_clef_example_request_serializes_to_the_documented_body() {
        let expected: serde_json::Value = serde_json::from_str(EXAMPLE_REQUEST).unwrap();
        assert_eq!(serde_json::to_value(example_request()).unwrap(), expected);
        let parsed: DecisionRequest = serde_json::from_str(EXAMPLE_REQUEST).unwrap();
        assert_eq!(parsed, example_request());
    }

    #[test]
    fn the_clef_example_response_deserializes_with_its_numbers() {
        let response: DecisionResponse = serde_json::from_str(EXAMPLE_RESPONSE).unwrap();
        assert_eq!(response.model, "clef");
        assert_eq!(
            response.usage,
            Usage {
                input_tokens: 332,
                output_tokens: 0
            }
        );
        let Answer::Choice {
            choice,
            probabilities,
            confidence,
        } = &response.answers["team"]
        else {
            panic!("team must be a choice answer: {:?}", response.answers);
        };
        assert_eq!(choice, "billing");
        assert_eq!(probabilities["billing"], 0.9888);
        assert_eq!(probabilities["technical"], 0.0044);
        assert_eq!(probabilities["other"], 0.0068);
        assert_eq!(*confidence, 0.9667);
        assert_eq!(response.answers["refund"], Answer::Noul { noul: 0.9916 });
    }

    #[test]
    fn unknown_answer_types_and_fields_are_tolerated() {
        let response: DecisionResponse = serde_json::from_str(
            r#"{
              "model": "clef",
              "answers": {
                "priority": {"type": "score", "score": 7.5, "range": [0, 10]},
                "refund": {"type": "noul", "noul": 0.25, "extra": true}
              },
              "usage": {"input_tokens": 1, "output_tokens": 0},
              "created_at": "2026-10-21"
            }"#,
        )
        .unwrap();
        assert_eq!(response.answers["priority"], Answer::Other);
        assert_eq!(response.answers["refund"], Answer::Noul { noul: 0.25 });
    }

    #[test]
    fn noul_criteria_use_the_literal_false_and_true_keys() {
        let question = Question::Noul {
            instructions: "Is it done?".into(),
            criteria: Some(NoulCriteria {
                if_false: Some("work remains".into()),
                if_true: None,
            }),
        };
        assert_eq!(
            serde_json::to_value(question).unwrap(),
            serde_json::json!({
                "type": "noul",
                "instructions": "Is it done?",
                "criteria": {"false": "work remains"}
            })
        );
    }
}
