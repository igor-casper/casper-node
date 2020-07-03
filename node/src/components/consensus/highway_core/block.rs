use super::{state::State, traits::Context};

/// A block: Chains of blocks are the consensus values in the CBC Casper sense.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Block<C: Context> {
    /// The total number of ancestors, i.e. the height in the blockchain.
    pub(crate) height: u64,
    /// The payload, e.g. a list of transactions.
    pub(crate) values: Vec<C::ConsensusValue>,
    /// A skip list index of the block's ancestors.
    ///
    /// For every `p = 1 << i` that divides `height`, this contains an `i`-th entry pointing to the
    /// ancestor with `height - p`.
    pub(crate) skip_idx: Vec<C::Hash>,
}

impl<C: Context> Block<C> {
    /// Creates a new block with the given parent and values. Panics if parent does not exist.
    pub(crate) fn new(
        parent_hash: Option<C::Hash>,
        values: Vec<C::ConsensusValue>,
        state: &State<C>,
    ) -> Block<C> {
        let (parent, mut skip_idx) = match parent_hash {
            None => return Block::initial(values),
            Some(hash) => (state.block(&hash), vec![hash]),
        };
        let height = parent.height + 1;
        for i in 0..height.trailing_zeros() as usize {
            let ancestor = state.block(&skip_idx[i]);
            skip_idx.push(ancestor.skip_idx[i].clone());
        }
        Block {
            height,
            values,
            skip_idx,
        }
    }

    /// Returns the block's parent, or `None` if it has height 0.
    pub(crate) fn parent(&self) -> Option<&C::Hash> {
        self.skip_idx.first()
    }

    fn initial(values: Vec<C::ConsensusValue>) -> Block<C> {
        Block {
            height: 0,
            values,
            skip_idx: vec![],
        }
    }
}
