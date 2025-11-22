use anyhow::{Context, Result};
use blake3::{Hash, Hasher};

use crate::utils::HarmonicError;

const BLOCK_SIZE: usize = 8192;

pub struct MerkleTree {
    pub root: Hash,
    pub all_nodes: Vec<Vec<Hash>>,
    pub block_size: usize,
    pub input_file_size: usize,
}

fn hash_leaf(leaf: &[u8]) -> Hash {
    let mut hasher = Hasher::new();

    hasher.update(&[0x00]);
    hasher.update(leaf);
    hasher.finalize()
}

fn hash_nodes(left: Hash, right: Hash) -> Hash {
    let mut hasher = Hasher::new();

    hasher.update(&[0x01]);
    hasher.update(left.as_bytes());
    hasher.update(right.as_bytes());
    hasher.finalize()
}

pub fn generate_merkle_tree_for_bytes(data: Vec<u8>, compute_tree: bool) -> Result<MerkleTree> {
    if data.is_empty() {
        return Err(HarmonicError::InvalidInputError.into())
    }
    let mut input: Vec<Hash> = Vec::new();

    for c in data.chunks(BLOCK_SIZE) {
        input.push(hash_leaf(&c));
    }

    if let Some(&last_hash) = input.last() {
        while !input.len().is_power_of_two() {
            input.push(last_hash);
        }
    }

    let mut all_nodes: Vec<Vec<Hash>> = Vec::new();
        
    all_nodes.push(input);

    let mut current_layer_index = 0;
    
    while all_nodes[current_layer_index].len() > 1 {
        let prev_layer = &all_nodes[current_layer_index];
        let mut next_layer = Vec::with_capacity(prev_layer.len() / 2);

        for i in (0..prev_layer.len()).step_by(2) {
            next_layer.push(hash_nodes(prev_layer[i], prev_layer[i+1]));
        }

        all_nodes.push(next_layer);
        current_layer_index += 1;
    }

    let root = all_nodes.last().unwrap()[0];
    
    if !compute_tree {
        all_nodes.clear();
    }

    Ok(MerkleTree {
        root,
        all_nodes,
        block_size: BLOCK_SIZE,
        input_file_size: data.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merkle_tree_generation() {
        let data = vec![0u8; 8192 * 4]; 
        let tree = generate_merkle_tree_for_bytes(data, true).unwrap();

        // 4 leaves -> 2 nodes -> 1 root
        assert_eq!(tree.all_nodes.len(), 3);
        assert_eq!(tree.all_nodes[0].len(), 4);
        assert_eq!(tree.all_nodes[1].len(), 2);
        assert_eq!(tree.all_nodes[2].len(), 1);
        assert_eq!(tree.root, tree.all_nodes[2][0]);
    }

    #[test]
    fn test_merkle_tree_no_compute() {
        let data = vec![0u8; 8192 * 4];
        let tree = generate_merkle_tree_for_bytes(data, false).unwrap();

        assert!(tree.all_nodes.is_empty());
    }
}