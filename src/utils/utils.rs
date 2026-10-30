use crate::models::nodes::node::{Node, NodeType};
use indexmap::IndexMap;
use std::collections::HashSet;


pub fn filter_routers(nodes: &IndexMap<String, Node>) -> IndexMap<&String, &Node> {
    nodes
        .iter()
        .filter(|(_, n)| match n.node_type {
            NodeType::Router(_) => true,
            _ => false,
        })
        .collect()
}

pub fn extract_and_sort_common_parts(strings: Vec<&Vec<String>>) -> (Vec<String>, Vec<String>) {
    if strings.len() == 1 {
        return (Vec::new(), strings[0].clone());
    }

    let mut word_sets = HashSet::new();

    for string in strings {
        word_sets.insert(string);
    }

    let common_words = word_sets
        .iter()
        .fold(None, |acc: Option<HashSet<&str>>, hs| {
            let hs: HashSet<&str> = hs.iter().map(|s| s.as_str()).collect();
            acc
                .map(|a| a.intersection(&hs).map(|s| *s).collect())
                .or(Some(hs))
        })
        .unwrap_or_default();

    let all_words = word_sets
        .iter()
        .fold(None, |acc: Option<HashSet<&str>>, hs| {
            let hs: HashSet<&str> = hs.iter().map(|s| s.as_str()).collect();
            acc
                .map(|a| a.union(&hs).map(|s| *s).collect())
                .or(Some(hs))
        })
        .unwrap_or_default();


    let mut non_common_words: Vec<String> = all_words.difference(&common_words).map(|s| s.to_string()).collect();
    let mut common_words: Vec<String> = common_words.iter().map(|s| s.to_string()).collect();

    non_common_words.sort_by(|a, b| a.cmp(&b));
    common_words.sort_by(|a, b| a.cmp(&b));

    (common_words, non_common_words)
}