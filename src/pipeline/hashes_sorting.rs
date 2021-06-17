use std::io::Cursor;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use crate::fast_rand_bool::FastRandBool;
use crate::hash::{HashFunctionFactory, HashTraitType};
use crate::hash_entry::{Direction, HashEntry};
use crate::pipeline::Pipeline;
use crate::unitig_link::{UnitigFlags, UnitigIndex, UnitigLink};
use crate::vec_slice::VecSlice;
use crate::DEFAULT_BUFFER_SIZE;
use parallel_processor::binary_writer::{BinaryWriter, StorageMode};
use parallel_processor::memory_data_size::MemoryDataSize;
use parallel_processor::multi_thread_buckets::{BucketsThreadDispatcher, MultiThreadBuckets};
use parallel_processor::smart_bucket_sort::{smart_radix_sort, SortKey};
use rand::{thread_rng, RngCore};
use rayon::iter::IndexedParallelIterator;
use rayon::iter::IntoParallelRefIterator;
use rayon::iter::ParallelIterator;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::marker::PhantomData;

impl Pipeline {
    pub fn hashes_sorting<H: HashFunctionFactory, P: AsRef<Path>>(
        file_hashes_inputs: Vec<PathBuf>,
        output_dir: P,
        buckets_count: usize,
    ) -> Vec<PathBuf> {
        let mut links_buckets = MultiThreadBuckets::<BinaryWriter>::new(
            buckets_count,
            &(
                output_dir.as_ref().join("links"),
                StorageMode::Plain {
                    buffer_size: DEFAULT_BUFFER_SIZE,
                },
            ),
            None,
        );

        file_hashes_inputs
            .par_iter()
            .enumerate()
            .for_each(|(index, input)| {
                let mut links_tmp = BucketsThreadDispatcher::new(
                    MemoryDataSize::from_kibioctets(64.0),
                    &links_buckets,
                );

                let mut rand_bool = FastRandBool::new();

                let file = filebuffer::FileBuffer::open(input).unwrap();

                let mut reader = Cursor::new(file.deref());
                let mut vec: Vec<HashEntry<H::HashType>> = Vec::new();

                while let Ok(value) = bincode::deserialize_from(&mut reader) {
                    vec.push(value);
                }

                struct Compare<H: HashFunctionFactory> {
                    _phantom: PhantomData<H>,
                }
                impl<H: HashFunctionFactory> SortKey<HashEntry<H::HashType>> for Compare<H> {
                    type KeyType = H::HashType;
                    const KEY_BITS: usize = 64;

                    #[inline(always)]
                    fn get(value: &HashEntry<H::HashType>) -> H::HashType {
                        value.hash
                    }

                    #[inline(always)]
                    fn get_shifted(value: &HashEntry<H::HashType>, rhs: u8) -> u8 {
                        H::get_shifted(value.hash, rhs) as u8
                    }
                }

                // vec.sort_unstable_by_key(|e| e.hash);
                smart_radix_sort::<_, Compare<H>, false>(&mut vec[..]);

                let mut unitigs_vec = Vec::new();

                for x in vec.group_by(|a, b| a.hash == b.hash) {
                    if x.len() == 2 && x[0].direction != x[1].direction {
                        let (fw, bw) = match x[0].direction {
                            Direction::Forward => (0, 1),
                            Direction::Backward => (1, 0),
                        };

                        let (slice_fw, slice_bw) = if rand_bool.get_randbool() {
                            unitigs_vec.push(UnitigIndex::new(x[bw].bucket, x[bw].entry as usize));
                            (VecSlice::new(unitigs_vec.len() - 1, 1), VecSlice::EMPTY)
                        } else {
                            unitigs_vec.push(UnitigIndex::new(x[fw].bucket, x[fw].entry as usize));
                            (VecSlice::EMPTY, VecSlice::new(unitigs_vec.len() - 1, 1))
                        };

                        links_tmp.add_element(
                            x[fw].bucket,
                            &unitigs_vec,
                            UnitigLink {
                                entry: x[fw].entry,
                                flags: UnitigFlags::new_direction(true),
                                entries: slice_fw,
                            },
                        );

                        links_tmp.add_element(
                            x[bw].bucket,
                            &unitigs_vec,
                            UnitigLink {
                                entry: x[bw].entry,
                                flags: UnitigFlags::new_direction(false),
                                entries: slice_bw,
                            },
                        );
                    }
                }
                links_tmp.finalize();
                println!("Done {}!", index);
            });
        links_buckets.finalize()
    }
}
