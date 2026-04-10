use crate::headers::request::HeaderOp;
use crate::headers::variables::VariableContext;
use cdn_common::HeaderRule;

/// Apply custom response header rules.
/// Same logic as request rules — returns resolved HeaderOps.
pub fn apply_response_rules(rules: &[HeaderRule], vars: &VariableContext) -> Vec<HeaderOp> {
    rules
        .iter()
        .map(|rule| {
            let value = rule.value.as_deref().map(|v| vars.substitute(v));
            HeaderOp {
                action: rule.action.clone(),
                name: rule.name.clone(),
                value,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdn_common::HeaderAction;

    fn rule(action: HeaderAction, name: &str, value: Option<&str>) -> HeaderRule {
        HeaderRule {
            action,
            name: name.to_string(),
            value: value.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_response_set() {
        let rules = vec![rule(HeaderAction::Set, "X-Frame-Options", Some("DENY"))];
        let vars = VariableContext::new();
        let ops = apply_response_rules(&rules, &vars);
        assert_eq!(ops[0].value.as_deref(), Some("DENY"));
    }

    #[test]
    fn test_response_variable() {
        let rules = vec![rule(HeaderAction::Set, "X-Cache", Some("${cache_status}"))];
        let mut vars = VariableContext::new();
        vars.set("cache_status", "HIT".to_string());
        let ops = apply_response_rules(&rules, &vars);
        assert_eq!(ops[0].value.as_deref(), Some("HIT"));
    }

    #[test]
    fn test_response_append() {
        let rules = vec![rule(HeaderAction::Append, "X-Via", Some("cdn-node-1"))];
        let vars = VariableContext::new();
        let ops = apply_response_rules(&rules, &vars);
        assert!(matches!(ops[0].action, HeaderAction::Append));
    }
}
