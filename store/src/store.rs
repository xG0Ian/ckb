use crate::{
    COLUMN_BLOCK_BODY, COLUMN_BLOCK_EPOCH, COLUMN_BLOCK_EXT, COLUMN_BLOCK_HEADER,
    COLUMN_BLOCK_PROPOSAL_IDS, COLUMN_BLOCK_UNCLE, COLUMN_CELL_META, COLUMN_CELL_SET, COLUMN_EPOCH,
    COLUMN_INDEX, COLUMN_META, COLUMN_TRANSACTION_ADDR, COLUMN_UNCLES,
};
use bincode::{deserialize, serialize};
use ckb_chain_spec::consensus::Consensus;
use ckb_core::block::{Block, BlockBuilder};
use ckb_core::cell::{BlockInfo, CellMeta};
use ckb_core::extras::{
    BlockExt, DaoStats, EpochExt, TransactionAddress, DEFAULT_ACCUMULATED_RATE,
};
use ckb_core::header::{BlockNumber, Header};
use ckb_core::transaction::{CellKey, CellOutPoint, CellOutput, ProposalShortId, Transaction};
use ckb_core::transaction_meta::TransactionMeta;
use ckb_core::uncle::UncleBlock;
use ckb_core::{Capacity, EpochNumber};
use ckb_db::{Col, DbBatch, Error, KeyValueDB};
use lru_cache::LruCache;
use numext_fixed_hash::H256;
use serde_derive::{Deserialize, Serialize};
use std::ops::Range;
use std::sync::Mutex;

const META_TIP_HEADER_KEY: &[u8] = b"TIP_HEADER";
const META_CURRENT_EPOCH_KEY: &[u8] = b"CURRENT_EPOCH";

#[derive(Clone, Serialize, Deserialize, Eq, PartialEq, Hash, Debug)]
pub struct StoreConfig {
    pub header_cache_size: usize,
    pub cell_output_cache_size: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            header_cache_size: 4096,
            cell_output_cache_size: 128,
        }
    }
}

pub struct ChainKVStore<T> {
    db: T,
    header_cache: Mutex<LruCache<H256, Header>>,
    cell_output_cache: Mutex<LruCache<(H256, u32), CellOutput>>,
}

impl<T: KeyValueDB> ChainKVStore<T> {
    pub fn new(db: T) -> Self {
        Self::with_config(db, StoreConfig::default())
    }

    pub fn with_config(db: T, config: StoreConfig) -> Self {
        ChainKVStore {
            db,
            header_cache: Mutex::new(LruCache::new(config.header_cache_size)),
            cell_output_cache: Mutex::new(LruCache::new(config.cell_output_cache_size)),
        }
    }

    pub fn get(&self, col: Col, key: &[u8]) -> Option<Vec<u8>> {
        self.db.read(col, key).expect("db operation should be ok")
    }

    pub fn partial_get(&self, col: Col, key: &[u8], range: &Range<usize>) -> Option<Vec<u8>> {
        self.db
            .partial_read(col, key, range)
            .expect("db operation should be ok")
    }

    fn process_get<F, Ret>(&self, col: Col, key: &[u8], process: F) -> Option<Ret>
    where
        F: FnOnce(&[u8]) -> Result<Option<Ret>, Error>,
    {
        self.db
            .process_read(col, key, process)
            .expect("db operation should be ok")
    }

    pub fn traverse<F>(&self, col: Col, callback: F) -> Result<(), Error>
    where
        F: FnMut(&[u8], &[u8]) -> Result<(), Error>,
    {
        self.db.traverse(col, callback)
    }
}

/// Store interface by chain
pub trait ChainStore: Sync + Send {
    /// Batch handle
    type Batch: StoreBatch;
    /// New a store batch handle
    fn new_batch(&self) -> Result<Self::Batch, Error>;

    /// Get block by block header hash
    fn get_block(&self, block_hash: &H256) -> Option<Block>;
    /// Get header by block header hash
    fn get_block_header(&self, block_hash: &H256) -> Option<Header>;
    /// Get block body by block header hash
    fn get_block_body(&self, block_hash: &H256) -> Option<Vec<Transaction>>;
    /// Get all transaction-hashes in block body by block header hash
    fn get_block_txs_hashes(&self, block_hash: &H256) -> Option<Vec<H256>>;
    /// Get proposal short id by block header hash
    fn get_block_proposal_txs_ids(&self, h: &H256) -> Option<Vec<ProposalShortId>>;
    /// Get block uncles by block header hash
    fn get_block_uncles(&self, block_hash: &H256) -> Option<Vec<UncleBlock>>;
    /// Get block ext by block header hash
    fn get_block_ext(&self, block_hash: &H256) -> Option<BlockExt>;

    fn init(&self, consensus: &Consensus) -> Result<(), Error>;
    /// Get block header hash by block number
    fn get_block_hash(&self, number: BlockNumber) -> Option<H256>;
    /// Get block number by block header hash
    fn get_block_number(&self, hash: &H256) -> Option<BlockNumber>;
    /// Get the tip(highest) header
    fn get_tip_header(&self) -> Option<Header>;
    /// Get commit transaction and block hash by it's hash
    fn get_transaction(&self, h: &H256) -> Option<(Transaction, H256)>;
    fn get_transaction_address(&self, hash: &H256) -> Option<TransactionAddress>;
    fn get_cell_meta(&self, tx_hash: &H256, index: u32) -> Option<CellMeta>;
    fn get_cell_output(&self, tx_hash: &H256, index: u32) -> Option<CellOutput>;
    // Get current epoch ext
    fn get_current_epoch_ext(&self) -> Option<EpochExt>;
    // Get epoch ext by epoch index
    fn get_epoch_ext(&self, hash: &H256) -> Option<EpochExt>;
    // Get epoch index by epoch number
    fn get_epoch_index(&self, number: EpochNumber) -> Option<H256>;
    // Get epoch index by block hash
    fn get_block_epoch_index(&self, h256: &H256) -> Option<H256>;
    fn traverse_cell_set<F>(&self, callback: F) -> Result<(), Error>
    where
        F: FnMut(H256, TransactionMeta) -> Result<(), Error>;
    fn is_uncle(&self, hash: &H256) -> bool;
    // Get cellbase by block hash
    fn get_cellbase(&self, hash: &H256) -> Option<Transaction>;
}

pub trait StoreBatch {
    fn insert_block(&mut self, block: &Block) -> Result<(), Error>;
    fn insert_block_ext(&mut self, block_hash: &H256, ext: &BlockExt) -> Result<(), Error>;
    fn insert_tip_header(&mut self, header: &Header) -> Result<(), Error>;
    fn insert_current_epoch_ext(&mut self, epoch: &EpochExt) -> Result<(), Error>;
    fn insert_block_epoch_index(
        &mut self,
        block_hash: &H256,
        epoch_hash: &H256,
    ) -> Result<(), Error>;
    fn insert_epoch_ext(&mut self, hash: &H256, epoch: &EpochExt) -> Result<(), Error>;

    fn attach_block(&mut self, block: &Block) -> Result<(), Error>;
    fn detach_block(&mut self, block: &Block) -> Result<(), Error>;

    fn update_cell_set(&mut self, tx_hash: &H256, meta: &TransactionMeta) -> Result<(), Error>;
    fn delete_cell_set(&mut self, tx_hash: &H256) -> Result<(), Error>;

    fn commit(self) -> Result<(), Error>;
}

impl<T: KeyValueDB> ChainStore for ChainKVStore<T> {
    type Batch = DefaultStoreBatch<T::Batch>;

    fn new_batch(&self) -> Result<Self::Batch, Error> {
        Ok(DefaultStoreBatch {
            inner: self.db.batch()?,
        })
    }

    fn get_block(&self, h: &H256) -> Option<Block> {
        self.get_block_header(h).map(|header| {
            let transactions = self
                .get_block_body(h)
                .expect("block transactions must be stored");
            let uncles = self
                .get_block_uncles(h)
                .expect("block uncles must be stored");
            let proposals = self
                .get_block_proposal_txs_ids(h)
                .expect("block proposal_ids must be stored");
            BlockBuilder::default()
                .header(header)
                .uncles(uncles)
                .transactions(transactions)
                .proposals(proposals)
                .build()
        })
    }

    fn is_uncle(&self, hash: &H256) -> bool {
        self.get(COLUMN_UNCLES, hash.as_bytes()).is_some()
    }

    fn get_block_header(&self, hash: &H256) -> Option<Header> {
        let mut header_cache_unlocked = self
            .header_cache
            .lock()
            .expect("poisoned header cache lock");
        if let Some(header) = header_cache_unlocked.get_refresh(hash) {
            return Some(header.clone());
        }
        // release lock asap
        drop(header_cache_unlocked);

        self.process_get(COLUMN_BLOCK_HEADER, hash.as_bytes(), |slice| {
            let header: Header = flatbuffers::get_root::<ckb_protos::StoredHeader>(&slice).into();
            Ok(Some(header))
        })
        .and_then(|header| {
            let mut header_cache_unlocked = self
                .header_cache
                .lock()
                .expect("poisoned header cache lock");
            header_cache_unlocked.insert(hash.clone(), header.clone());
            Some(header)
        })
    }

    fn get_block_uncles(&self, hash: &H256) -> Option<Vec<UncleBlock>> {
        self.process_get(COLUMN_BLOCK_UNCLE, hash.as_bytes(), |slice| {
            let uncles: Vec<UncleBlock> =
                flatbuffers::get_root::<ckb_protos::StoredUncleBlocks>(&slice).into();
            Ok(Some(uncles))
        })
    }

    fn get_block_proposal_txs_ids(&self, hash: &H256) -> Option<Vec<ProposalShortId>> {
        self.process_get(COLUMN_BLOCK_PROPOSAL_IDS, hash.as_bytes(), |slice| {
            let uncles: Vec<ProposalShortId> =
                flatbuffers::get_root::<ckb_protos::StoredProposalShortIds>(&slice).into();
            Ok(Some(uncles))
        })
    }

    fn get_block_body(&self, hash: &H256) -> Option<Vec<Transaction>> {
        self.process_get(COLUMN_BLOCK_BODY, hash.as_bytes(), |slice| {
            let transactions: Vec<Transaction> =
                flatbuffers::get_root::<ckb_protos::StoredBlockBody>(&slice).into();
            Ok(Some(transactions))
        })
    }

    fn get_block_txs_hashes(&self, hash: &H256) -> Option<Vec<H256>> {
        self.process_get(COLUMN_BLOCK_BODY, hash.as_bytes(), |slice| {
            let tx_hashes =
                flatbuffers::get_root::<ckb_protos::StoredBlockBody>(&slice).tx_hashes();
            Ok(Some(tx_hashes))
        })
    }

    fn get_block_ext(&self, block_hash: &H256) -> Option<BlockExt> {
        self.process_get(COLUMN_BLOCK_EXT, block_hash.as_bytes(), |slice| {
            let ext: BlockExt = flatbuffers::get_root::<ckb_protos::BlockExt>(&slice).into();
            Ok(Some(ext))
        })
    }

    fn init(&self, consensus: &Consensus) -> Result<(), Error> {
        let genesis = consensus.genesis_block();
        let epoch = consensus.genesis_epoch_ext();
        let mut batch = self.new_batch()?;
        let genesis_hash = genesis.header().hash();
        let ext = BlockExt {
            received_at: genesis.header().timestamp(),
            total_difficulty: genesis.header().difficulty().clone(),
            total_uncles_count: 0,
            verified: Some(true),
            txs_fees: vec![],
            dao_stats: DaoStats {
                accumulated_rate: DEFAULT_ACCUMULATED_RATE,
                accumulated_capacity: genesis
                    .transactions()
                    .get(0)
                    .map(|tx| {
                        tx.outputs()
                            .iter()
                            .skip(1)
                            .try_fold(Capacity::zero(), |capacity, output| {
                                capacity.safe_add(output.capacity)
                            })
                            .expect("accumulated capacity in genesis block should not overflow")
                    })
                    .unwrap_or_else(Capacity::zero)
                    .as_u64(),
            },
        };

        let mut cells = Vec::with_capacity(genesis.transactions().len());

        for tx in genesis.transactions() {
            let tx_meta;
            let ins = if tx.is_cellbase() {
                tx_meta = TransactionMeta::new_cellbase(
                    genesis.header().number(),
                    genesis.header().epoch(),
                    tx.outputs().len(),
                    false,
                );
                Vec::new()
            } else {
                tx_meta = TransactionMeta::new(
                    genesis.header().number(),
                    genesis.header().epoch(),
                    tx.outputs().len(),
                    false,
                );
                tx.input_pts_iter().cloned().collect()
            };
            batch.update_cell_set(tx.hash(), &tx_meta)?;
            let outs = tx.output_pts();

            cells.push((ins, outs));
        }

        batch.insert_block(genesis)?;
        batch.insert_block_ext(&genesis_hash, &ext)?;
        batch.insert_tip_header(&genesis.header())?;
        batch.insert_current_epoch_ext(epoch)?;
        batch.insert_block_epoch_index(&genesis_hash, epoch.last_block_hash_in_previous_epoch())?;
        batch.insert_epoch_ext(epoch.last_block_hash_in_previous_epoch(), &epoch)?;
        batch.attach_block(genesis)?;
        batch.commit()
    }

    fn get_block_hash(&self, number: BlockNumber) -> Option<H256> {
        self.get(COLUMN_INDEX, &number.to_le_bytes())
            .map(|raw| H256::from_slice(&raw[..]).expect("db safe access"))
    }

    fn get_block_number(&self, hash: &H256) -> Option<BlockNumber> {
        self.get(COLUMN_INDEX, hash.as_bytes())
            .map(|raw| deserialize(&raw[..]).expect("deserialize block number should be ok"))
    }

    fn get_tip_header(&self) -> Option<Header> {
        self.get(COLUMN_META, META_TIP_HEADER_KEY)
            .and_then(|raw| {
                self.get_block_header(&H256::from_slice(&raw[..]).expect("db safe access"))
            })
            .map(Into::into)
    }

    fn get_current_epoch_ext(&self) -> Option<EpochExt> {
        self.process_get(COLUMN_META, META_CURRENT_EPOCH_KEY, |slice| {
            let ext: EpochExt = flatbuffers::get_root::<ckb_protos::StoredEpochExt>(&slice).into();
            Ok(Some(ext))
        })
    }

    fn get_epoch_ext(&self, hash: &H256) -> Option<EpochExt> {
        self.process_get(COLUMN_EPOCH, hash.as_bytes(), |slice| {
            let ext: EpochExt = flatbuffers::get_root::<ckb_protos::StoredEpochExt>(&slice).into();
            Ok(Some(ext))
        })
    }

    fn get_epoch_index(&self, number: EpochNumber) -> Option<H256> {
        self.get(COLUMN_EPOCH, &number.to_le_bytes())
            .map(|raw| H256::from_slice(&raw[..]).expect("db safe access"))
    }

    fn get_block_epoch_index(&self, block_hash: &H256) -> Option<H256> {
        self.get(COLUMN_BLOCK_EPOCH, block_hash.as_bytes())
            .map(|raw| H256::from_slice(&raw[..]).expect("db safe access"))
    }

    fn get_transaction(&self, hash: &H256) -> Option<(Transaction, H256)> {
        self.get_transaction_address(&hash).and_then(|addr| {
            self.process_get(COLUMN_BLOCK_BODY, addr.block_hash.as_bytes(), |slice| {
                let tx_opt = flatbuffers::get_root::<ckb_protos::StoredBlockBody>(&slice)
                    .transaction(addr.index);
                Ok(tx_opt)
            })
            .map(|tx| (tx, addr.block_hash))
        })
    }

    fn get_transaction_address(&self, hash: &H256) -> Option<TransactionAddress> {
        self.process_get(COLUMN_TRANSACTION_ADDR, hash.as_bytes(), |slice| {
            let addr: TransactionAddress =
                flatbuffers::get_root::<ckb_protos::StoredTransactionAddress>(&slice).into();
            Ok(Some(addr))
        })
    }

    fn get_cell_meta(&self, tx_hash: &H256, index: u32) -> Option<CellMeta> {
        self.get(
            COLUMN_CELL_META,
            CellKey::calculate(tx_hash, index).as_ref(),
        )
        .map(|raw| deserialize(&raw[..]).expect("deserialize cell meta should be ok"))
    }

    fn get_cellbase(&self, hash: &H256) -> Option<Transaction> {
        self.process_get(COLUMN_BLOCK_BODY, hash.as_bytes(), |slice| {
            let cellbase = flatbuffers::get_root::<ckb_protos::StoredBlockBody>(&slice)
                .transaction(0)
                .expect("cellbase address should exist");
            Ok(Some(cellbase))
        })
    }

    fn get_cell_output(&self, tx_hash: &H256, index: u32) -> Option<CellOutput> {
        let mut cell_output_cache_unlocked = self
            .cell_output_cache
            .lock()
            .expect("poisoned cell output cache lock");
        if let Some(cell_output) = cell_output_cache_unlocked.get_refresh(&(tx_hash.clone(), index))
        {
            return Some(cell_output.clone());
        }
        // release lock asap
        drop(cell_output_cache_unlocked);

        self.get_transaction_address(&tx_hash)
            .and_then(|addr| {
                self.process_get(COLUMN_BLOCK_BODY, addr.block_hash.as_bytes(), |slice| {
                    let output_opt = flatbuffers::get_root::<ckb_protos::StoredBlockBody>(&slice)
                        .output(addr.index, index as usize);
                    Ok(output_opt)
                })
            })
            .map(|cell_output: CellOutput| {
                let mut cell_output_cache_unlocked = self
                    .cell_output_cache
                    .lock()
                    .expect("poisoned cell output cache lock");
                cell_output_cache_unlocked.insert((tx_hash.clone(), index), cell_output.clone());
                cell_output
            })
    }

    fn traverse_cell_set<F>(&self, mut callback: F) -> Result<(), Error>
    where
        F: FnMut(H256, TransactionMeta) -> Result<(), Error>,
    {
        self.traverse(COLUMN_CELL_SET, |hash_slice, tx_meta_bytes| {
            let tx_hash = H256::from_slice(hash_slice).expect("deserialize tx hash should be ok");
            let tx_meta: TransactionMeta =
                flatbuffers::get_root::<ckb_protos::TransactionMeta>(tx_meta_bytes).into();
            callback(tx_hash, tx_meta)
        })
    }
}

pub struct DefaultStoreBatch<B> {
    inner: B,
}

/// helper methods
impl<B: DbBatch> DefaultStoreBatch<B> {
    fn insert_raw(&mut self, col: Col, key: &[u8], value: &[u8]) -> Result<(), Error> {
        self.inner.insert(col, key, value)
    }

    fn insert_serialize<T: serde::ser::Serialize + ?Sized>(
        &mut self,
        col: Col,
        key: &[u8],
        item: &T,
    ) -> Result<(), Error> {
        self.inner.insert(
            col,
            key,
            &serialize(item).expect("serializing should be ok"),
        )
    }

    fn delete(&mut self, col: Col, key: &[u8]) -> Result<(), Error> {
        self.inner.delete(col, key)
    }
}

macro_rules! insert_flatbuffers {
    ($database:ident, $col:ident, $key:expr, $type:ident, $data:expr) => {
        let builder = &mut flatbuffers::FlatBufferBuilder::new();
        let proto = ckb_protos::$type::build(builder, $data);
        builder.finish(proto, None);
        let slice = builder.finished_data();
        $database.insert_raw($col, $key, slice)?;
    };
}

impl<B: DbBatch> StoreBatch for DefaultStoreBatch<B> {
    fn insert_block(&mut self, block: &Block) -> Result<(), Error> {
        let hash = block.header().hash().as_bytes();
        insert_flatbuffers!(
            self,
            COLUMN_BLOCK_HEADER,
            hash,
            StoredHeader,
            block.header()
        );
        insert_flatbuffers!(
            self,
            COLUMN_BLOCK_UNCLE,
            hash,
            StoredUncleBlocks,
            block.uncles()
        );
        insert_flatbuffers!(
            self,
            COLUMN_BLOCK_PROPOSAL_IDS,
            hash,
            StoredProposalShortIds,
            block.proposals()
        );
        insert_flatbuffers!(
            self,
            COLUMN_BLOCK_BODY,
            hash,
            StoredBlockBody,
            block.transactions()
        );
        Ok(())
    }

    fn insert_block_ext(&mut self, block_hash: &H256, ext: &BlockExt) -> Result<(), Error> {
        insert_flatbuffers!(self, COLUMN_BLOCK_EXT, block_hash.as_bytes(), BlockExt, ext);
        Ok(())
    }

    fn attach_block(&mut self, block: &Block) -> Result<(), Error> {
        let hash = block.header().hash();
        for (index, tx) in block.transactions().iter().enumerate() {
            let tx_hash = tx.hash();
            {
                let addr = TransactionAddress {
                    block_hash: hash.to_owned(),
                    index,
                };
                insert_flatbuffers!(
                    self,
                    COLUMN_TRANSACTION_ADDR,
                    tx_hash.as_bytes(),
                    StoredTransactionAddress,
                    &addr
                );
            }
            let cellbase = index == 0;
            for (index, output) in tx.outputs().iter().enumerate() {
                let out_point = CellOutPoint {
                    tx_hash: tx_hash.to_owned(),
                    index: index as u32,
                };
                let store_key = out_point.cell_key();
                let cell_meta = CellMeta {
                    cell_output: None,
                    out_point,
                    block_info: Some(BlockInfo {
                        number: block.header().number(),
                        epoch: block.header().epoch(),
                    }),
                    cellbase,
                    capacity: output.capacity,
                    data_hash: Some(output.data_hash()),
                };
                self.insert_serialize(COLUMN_CELL_META, store_key.as_ref(), &cell_meta)?;
            }
        }

        let number = block.header().number().to_le_bytes();
        self.insert_raw(COLUMN_INDEX, &number, hash.as_bytes())?;
        for uncle in block.uncles() {
            self.insert_raw(COLUMN_UNCLES, &uncle.hash().as_bytes(), &[])?;
        }
        self.insert_raw(COLUMN_INDEX, hash.as_bytes(), &number)
    }

    fn detach_block(&mut self, block: &Block) -> Result<(), Error> {
        for tx in block.transactions() {
            let tx_hash = tx.hash();
            self.delete(COLUMN_TRANSACTION_ADDR, tx_hash.as_bytes())?;
            for index in 0..tx.outputs().len() {
                let store_key = CellKey::calculate(&tx_hash, index as u32);
                self.delete(COLUMN_CELL_META, store_key.as_ref())?;
            }
        }

        for uncle in block.uncles() {
            self.delete(COLUMN_UNCLES, &uncle.hash().as_bytes())?;
        }
        self.delete(COLUMN_INDEX, &block.header().number().to_le_bytes())?;
        self.delete(COLUMN_INDEX, block.header().hash().as_bytes())
    }

    fn insert_tip_header(&mut self, h: &Header) -> Result<(), Error> {
        self.insert_raw(COLUMN_META, META_TIP_HEADER_KEY, h.hash().as_bytes())
    }

    fn insert_block_epoch_index(
        &mut self,
        block_hash: &H256,
        epoch_hash: &H256,
    ) -> Result<(), Error> {
        self.insert_raw(
            COLUMN_BLOCK_EPOCH,
            block_hash.as_bytes(),
            epoch_hash.as_bytes(),
        )
    }

    fn insert_epoch_ext(&mut self, hash: &H256, epoch: &EpochExt) -> Result<(), Error> {
        let epoch_index = hash.as_bytes();
        let epoch_number = epoch.number().to_le_bytes();
        insert_flatbuffers!(self, COLUMN_EPOCH, epoch_index, StoredEpochExt, epoch);
        self.insert_raw(COLUMN_EPOCH, &epoch_number, epoch_index)
    }

    fn insert_current_epoch_ext(&mut self, epoch: &EpochExt) -> Result<(), Error> {
        insert_flatbuffers!(
            self,
            COLUMN_META,
            META_CURRENT_EPOCH_KEY,
            StoredEpochExt,
            epoch
        );
        Ok(())
    }

    fn update_cell_set(&mut self, tx_hash: &H256, meta: &TransactionMeta) -> Result<(), Error> {
        insert_flatbuffers!(
            self,
            COLUMN_CELL_SET,
            tx_hash.as_bytes(),
            TransactionMeta,
            meta
        );
        Ok(())
    }

    fn delete_cell_set(&mut self, tx_hash: &H256) -> Result<(), Error> {
        self.delete(COLUMN_CELL_SET, tx_hash.as_bytes())
    }

    fn commit(self) -> Result<(), Error> {
        self.inner.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::super::COLUMNS;
    use super::*;
    use crate::store::StoreBatch;
    use ckb_chain_spec::consensus::Consensus;
    use ckb_core::transaction::TransactionBuilder;
    use ckb_db::{DBConfig, RocksDB};
    use tempfile;

    fn setup_db(prefix: &str, columns: u32) -> RocksDB {
        let tmp_dir = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
        let config = DBConfig {
            path: tmp_dir.as_ref().to_path_buf(),
            ..Default::default()
        };

        RocksDB::open(&config, columns)
    }

    #[test]
    fn save_and_get_block() {
        let db = setup_db("save_and_get_block", COLUMNS);
        let store = ChainKVStore::new(db);
        let consensus = Consensus::default();
        let block = consensus.genesis_block();

        let hash = block.header().hash();
        let mut batch = store.new_batch().unwrap();
        batch.insert_block(&block).unwrap();
        batch.commit().unwrap();
        assert_eq!(block, &store.get_block(&hash).unwrap());
    }

    #[test]
    fn save_and_get_block_with_transactions() {
        let db = setup_db("save_and_get_block_with_transactions", COLUMNS);
        let store = ChainKVStore::new(db);
        let block = BlockBuilder::default()
            .transaction(TransactionBuilder::default().build())
            .transaction(TransactionBuilder::default().build())
            .transaction(TransactionBuilder::default().build())
            .build();

        let hash = block.header().hash();
        let mut batch = store.new_batch().unwrap();
        batch.insert_block(&block).unwrap();
        batch.commit().unwrap();
        assert_eq!(block, store.get_block(&hash).unwrap());
    }

    #[test]
    fn save_and_get_block_ext() {
        let db = setup_db("save_and_get_block_ext", COLUMNS);
        let store = ChainKVStore::new(db);
        let consensus = Consensus::default();
        let block = consensus.genesis_block();

        let ext = BlockExt {
            received_at: block.header().timestamp(),
            total_difficulty: block.header().difficulty().to_owned(),
            total_uncles_count: block.uncles().len() as u64,
            verified: Some(true),
            txs_fees: vec![],
            dao_stats: DaoStats {
                accumulated_rate: DEFAULT_ACCUMULATED_RATE,
                accumulated_capacity: block.outputs_capacity().unwrap().as_u64(),
            },
        };

        let hash = block.header().hash();
        let mut batch = store.new_batch().unwrap();
        batch.insert_block_ext(&hash, &ext).unwrap();
        batch.commit().unwrap();
        assert_eq!(ext, store.get_block_ext(&hash).unwrap());
    }

    #[test]
    fn index_store() {
        let tmp_dir = tempfile::Builder::new()
            .prefix("index_init")
            .tempdir()
            .unwrap();
        let config = DBConfig {
            path: tmp_dir.as_ref().to_path_buf(),
            ..Default::default()
        };
        let db = RocksDB::open(&config, COLUMNS);
        let store = ChainKVStore::new(db);
        let consensus = Consensus::default();
        let block = consensus.genesis_block();
        let hash = block.header().hash();
        store.init(&consensus).unwrap();
        assert_eq!(hash, &store.get_block_hash(0).unwrap());

        assert_eq!(
            block.header().difficulty(),
            &store.get_block_ext(&hash).unwrap().total_difficulty
        );

        assert_eq!(
            block.header().number(),
            store.get_block_number(&hash).unwrap()
        );

        assert_eq!(block.header(), &store.get_tip_header().unwrap());
    }
}
