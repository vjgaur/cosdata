use super::file_persist::*;
use super::lazy_load::{FileIndex, LazyItem};
use super::serializer::CustomSerialize;
use super::types::*;
use arcshift::ArcShift;
use dashmap::DashMap;
use probabilistic_collections::cuckoo::CuckooFilter;
use std::collections::HashSet;
use std::io::{Read, Seek};
use std::sync::{atomic::AtomicBool, Arc, RwLock};

pub struct NodeRegistry<R: Read + Seek> {
    cuckoo_filter: RwLock<CuckooFilter<u64>>,
    registry: DashMap<u64, LazyItem<MergedNode>>,
    reader: Arc<RwLock<R>>,
}

impl<R: Read + Seek> NodeRegistry<R> {
    pub fn new(cuckoo_filter_capacity: usize, reader: R) -> Self {
        let cuckoo_filter = CuckooFilter::new(cuckoo_filter_capacity);
        let registry = DashMap::new();
        NodeRegistry {
            cuckoo_filter: RwLock::new(cuckoo_filter),
            registry,
            reader: Arc::new(RwLock::new(reader)),
        }
    }
    pub fn get_object<F>(
        self: Arc<Self>,
        file_index: FileIndex,
        reader: &mut R,
        load_function: F,
        max_loads: u16,
        skipm: &mut HashSet<u64>,
    ) -> std::io::Result<LazyItem<MergedNode>>
    where
        F: Fn(&mut R, FileIndex, Arc<Self>, u16, &mut HashSet<u64>) -> std::io::Result<MergedNode>,
    {
        println!(
            "get_object called with file_index: {:?}, max_loads: {}",
            file_index, max_loads
        );

        let combined_index = Self::combine_index(&file_index);

        {
            let cuckoo_filter = self.cuckoo_filter.read().unwrap();
            println!("Acquired read lock on cuckoo_filter");

            // Initial check with Cuckoo filter
            if cuckoo_filter.contains(&combined_index) {
                println!("FileIndex found in cuckoo_filter");
                if let Some(obj) = self.registry.get(&combined_index) {
                    println!("Object found in registry, returning");
                    return Ok(obj.clone());
                } else {
                    println!("Object not found in registry despite being in cuckoo_filter");
                }
            } else {
                println!("FileIndex not found in cuckoo_filter");
            }
        }
        println!("Released read lock on cuckoo_filter");

        let version_id = if let FileIndex::Valid { version, .. } = &file_index {
            *version
        } else {
            VersionId(0)
        };

        if max_loads == 0 || !skipm.insert(combined_index) {
            println!("Either max_loads hit 0 or loop detected, returning LazyItem with no data");
            return Ok(LazyItem::Valid {
                data: None,
                file_index: ArcShift::new(Some(file_index)),
                decay_counter: 0,
                persist_flag: Arc::new(AtomicBool::new(true)),
                version_id,
            });
        }

        println!("Calling load_function");
        let node = load_function(
            reader,
            file_index.clone(),
            self.clone(),
            max_loads - 1,
            skipm,
        )?;
        println!("load_function returned successfully");

        if let Some(obj) = self.registry.get(&combined_index) {
            println!("Object found in registry after load, returning");
            return Ok(obj.clone());
        }

        println!("Inserting key into cuckoo_filter");
        self.cuckoo_filter.write().unwrap().insert(&combined_index);

        let item = LazyItem::Valid {
            data: Some(ArcShift::new(node)),
            file_index: ArcShift::new(Some(file_index)),
            decay_counter: 0,
            persist_flag: Arc::new(AtomicBool::new(true)),
            version_id,
        };

        println!("Inserting item into registry");
        self.registry.insert(combined_index, item.clone());

        println!("Returning newly created LazyItem");
        Ok(item)
    }

    pub fn load_item<T: CustomSerialize>(
        self: Arc<Self>,
        file_index: FileIndex,
    ) -> std::io::Result<T> {
        let mut reader_lock = self.reader.write().unwrap();
        let mut skipm: HashSet<u64> = HashSet::new();

        if file_index == FileIndex::Invalid {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Cannot deserialize with an invalid FileIndex",
            ));
        };

        T::deserialize(
            &mut *reader_lock,
            file_index,
            self.clone(),
            1000,
            &mut skipm,
        )
    }

    pub fn combine_index(file_index: &FileIndex) -> u64 {
        match file_index {
            FileIndex::Valid { offset, version } => ((offset.0 as u64) << 32) | (version.0 as u64),
            FileIndex::Invalid => u64::MAX, // Use max u64 value for Invalid
        }
    }

    pub fn split_combined_index(combined: u64) -> FileIndex {
        if combined == u64::MAX {
            FileIndex::Invalid
        } else {
            FileIndex::Valid {
                offset: FileOffset((combined >> 32) as u32),
                version: VersionId(combined as u16),
            }
        }
    }
}

pub fn load_cache() {
    use std::fs::OpenOptions;

    let file = OpenOptions::new()
        .read(true)
        .open("0.index")
        .expect("failed to open");

    let file_index = FileIndex::Valid {
        offset: FileOffset(0),
        version: VersionId(0),
    }; // Assuming initial version is 0
    let cache = Arc::new(NodeRegistry::new(1000, file));
    match read_node_from_file(file_index.clone(), cache) {
        Ok(_) => println!(
            "Successfully read and printed node from file_index {:?}",
            file_index
        ),
        Err(e) => println!("Failed to read node: {}", e),
    }
}
