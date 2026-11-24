use ahash::AHashMap;
use std::{collections::hash_map::Entry, convert::TryFrom, hash::Hash, sync::Arc};

#[derive(Debug)]
pub struct TrieBuildError;
impl std::fmt::Display for TrieBuildError {
    fn fmt(&self, f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { f.write_str("Unigram trie exceeds checked u32 index capacity") }
}
impl std::error::Error for TrieBuildError {}

pub struct TrieBuilder<Label> { root:BuilderNode<Label>, nodes:usize, limit:usize, failed:bool }
impl<Label> Default for TrieBuilder<Label> {
    fn default()->Self { Self { root:BuilderNode::default(),nodes:1,limit:usize::try_from(u32::MAX).unwrap_or(usize::MAX),failed:false } }
}
struct BuilderNode<Label> { is_leaf:bool, children:AHashMap<Label,BuilderNode<Label>> }
impl<Label> Default for BuilderNode<Label> { fn default()->Self { Self { is_leaf:false,children:AHashMap::new() } } }
impl<Label> Drop for BuilderNode<Label> {
    fn drop(&mut self) {
        // Every removed node has its children taken before its own Drop. Even a
        // failed huge-token construction tears down without recursive stack use.
        let mut pending:Vec<_>=std::mem::take(&mut self.children).into_iter().map(|(_,value)|value).collect();
        while let Some(mut next)=pending.pop() {
            pending.extend(std::mem::take(&mut next.children).into_iter().map(|(_,value)|value));
        }
    }
}
struct Node { first_edge:u32, edge_count:u32, is_leaf:bool }
struct Edge<Label> { label:Label, child:u32 }
struct Graph<Label> { nodes:Box<[Node]>, edges:Box<[Edge<Label>]> }
// Existing Clone trait now shares a sealed immutable graph, count only. This
// new ownership semantic is disclosed for adoption review, not a payload clone.
#[derive(Clone)]
pub struct Trie<Label> { graph:Arc<Graph<Label>> }

impl<Label:Eq + Hash + Copy + Ord> TrieBuilder<Label> {
    pub fn push(&mut self, element:&[Label])->Result<(),TrieBuildError> {
        if self.failed || self.nodes>self.limit { self.failed=true;return Err(TrieBuildError); }
        let count=&mut self.nodes;let failed=&mut self.failed;let limit=self.limit;
        let mut node=&mut self.root;
        for label in element {
            node=match node.children.entry(*label) {
                Entry::Occupied(entry)=>entry.into_mut(),
                Entry::Vacant(entry)=> {
                    if *count>=limit { *failed=true;return Err(TrieBuildError); }
                    *count=count.checked_add(1).ok_or(TrieBuildError)?;
                    entry.insert(BuilderNode::default())
                }
            };
        }
        node.is_leaf=true;Ok(())
    }
    pub fn build(self)->Result<Trie<Label>,TrieBuildError> {
        if self.failed || self.nodes>self.limit { return Err(TrieBuildError); }
        let mut nodes=Vec::with_capacity(self.nodes);let mut edges=Vec::with_capacity(self.nodes.saturating_sub(1));
        nodes.push(Node { first_edge:0,edge_count:0,is_leaf:false });
        let mut pending=vec![(0usize,self.root)];
        while let Some((index,mut node))=pending.pop() {
            let mut children:Vec<_>=std::mem::take(&mut node.children).into_iter().collect();
            children.sort_unstable_by_key(|(label,_)|*label);
            let first=u32::try_from(edges.len()).map_err(|_|TrieBuildError)?;
            let count=u32::try_from(children.len()).map_err(|_|TrieBuildError)?;
            let child_start=nodes.len();let end=child_start.checked_add(children.len()).ok_or(TrieBuildError)?;
            if end>self.limit { return Err(TrieBuildError); }
            nodes[index]=Node { first_edge:first,edge_count:count,is_leaf:node.is_leaf };
            for (at,(label,child)) in children.iter().enumerate() {
                let child_index=u32::try_from(child_start.checked_add(at).ok_or(TrieBuildError)?).map_err(|_|TrieBuildError)?;
                nodes.push(Node { first_edge:0,edge_count:0,is_leaf:child.is_leaf });
                edges.push(Edge { label:*label,child:child_index });
            }
            for (at,(_,child)) in children.into_iter().enumerate().rev() { pending.push((child_start+at,child)); }
        }
        Ok(Trie { graph:Arc::new(Graph { nodes:nodes.into_boxed_slice(),edges:edges.into_boxed_slice() }) })
    }
}
impl<Label> Default for Trie<Label> {
    fn default()->Self { Self { graph:Arc::new(Graph {
        nodes:vec![Node { first_edge:0,edge_count:0,is_leaf:false }].into_boxed_slice(),edges:Vec::new().into_boxed_slice(),
    }) } }
}
impl<Label:Eq + Hash + Copy + Ord> Trie<Label> {
    pub fn common_prefix_search<T>(&self,iterator:T)->TrieIterator<'_,Label,T> where T:Iterator<Item=Label> {
        TrieIterator { graph:&self.graph,node:0,prefix:Vec::new(),iterator }
    }
}
pub struct TrieIterator<'a,Label,T> { graph:&'a Graph<Label>,node:u32,prefix:Vec<Label>,iterator:T }
impl<Label:Ord + Copy,T:Iterator<Item=Label>> Iterator for TrieIterator<'_,Label,T> {
    type Item=Vec<Label>;
    fn next(&mut self)->Option<Self::Item> {
        loop {
            let label=self.iterator.next()?;self.prefix.push(label);
            let node=self.graph.nodes.get(usize::try_from(self.node).ok()?)?;
            let start=usize::try_from(node.first_edge).ok()?;let count=usize::try_from(node.edge_count).ok()?;
            let edges=self.graph.edges.get(start..start.checked_add(count)?)?;
            let at=edges.binary_search_by_key(&label,|edge|edge.label).ok()?;self.node=edges.get(at)?.child;
            if self.graph.nodes.get(usize::try_from(self.node).ok()?)?.is_leaf {
                // Preserve the owned-prefix iterator contract with one explicit
                // bounded byte-prefix copy, never a vocabulary/model copy.
                return Some(Vec::from(self.prefix.as_slice()));
            }
        }
    }
}
#[cfg(feature="memory-layout-diagnostics")]
impl<Label> Trie<Label> {
    pub(crate) fn diagnostic_layout(&self)->serde_json::Value {
        serde_json::json!({"nodes":self.graph.nodes.len(),"edges":self.graph.edges.len(),
            "node_inline_bytes":std::mem::size_of::<Node>(),"edge_inline_bytes":std::mem::size_of::<Edge<Label>>(),
            "node_payload_bytes":self.graph.nodes.len()*std::mem::size_of::<Node>(),
            "edge_payload_bytes":self.graph.edges.len()*std::mem::size_of::<Edge<Label>>(),
            "graph_arc_strong_count":Arc::strong_count(&self.graph),"builder":"checked iterative traversal of original hash tree"})
    }
}
#[cfg(test)]
mod flat_tests {
    use super::*;
    #[test]
    fn prefix_order_and_empty_token() {
        let mut builder=TrieBuilder::default();for word in [b"".as_slice(),b"a",b"ab",b"abc",b"abc",b"b"] { builder.push(word).unwrap(); }
        let trie=builder.build().unwrap();
        assert_eq!(trie.common_prefix_search(b"abcd".iter().copied()).collect::<Vec<_>>(),vec![b"a".to_vec(),b"ab".to_vec(),b"abc".to_vec()]);
        assert!(trie.common_prefix_search(std::iter::empty()).next().is_none());
    }
    #[test]
    fn small_index_limit_propagates_without_partial_acceptance() {
        let mut builder=TrieBuilder::<u8>::default();builder.limit=4;
        assert!(builder.push(b"abc").is_ok());assert!(builder.push(b"d").is_err());assert!(builder.build().is_err());
    }
    #[test]
    fn long_token_iterative_build_and_failed_drop() {
        let token=vec![b'a';100_000];
        let mut builder=TrieBuilder::default();builder.push(&token).unwrap();
        let trie=builder.build().unwrap();assert_eq!(trie.common_prefix_search(token.iter().copied()).count(),1);drop(trie);
        let mut builder=TrieBuilder::default();builder.push(&token).unwrap();builder.limit=2;assert!(builder.build().is_err());
    }
}
