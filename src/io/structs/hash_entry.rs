use crate::config::BucketIndexType;
use crate::hashes::HashFunctionFactory;
use bincode::serialize_into;
use parallel_processor::buckets::bucket_writer::BucketWriter;
use parallel_processor::fast_smart_bucket_sort::SortKey;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::mem::size_of;

#[derive(Copy, Clone, Eq, PartialEq, Serialize, Deserialize, Debug)]
#[repr(u8)]
pub enum Direction {
    Forward,
    Backward,
}

#[derive(Copy, Clone, Serialize, Deserialize, Debug)]
pub struct HashEntry<H: Copy> {
    pub hash: H,
    pub bucket: BucketIndexType,
    pub entry: u64,
    pub direction: Direction,
}

impl<H: Serialize + DeserializeOwned + Copy> BucketWriter for HashEntry<H> {
    type ExtraData = ();

    #[inline(always)]
    fn write_to(&self, bucket: &mut Vec<u8>, _extra_data: &Self::ExtraData) {
        serialize_into(bucket, self).unwrap();
    }

    #[inline(always)]
    fn get_size(&self) -> usize {
        size_of::<H>() + size_of::<BucketIndexType>() + 8 + 1
    }
}

pub struct HashCompare<H: HashFunctionFactory> {
    _phantom: PhantomData<H>,
}

impl<H: HashFunctionFactory> SortKey<HashEntry<H::HashTypeUnextendable>> for HashCompare<H> {
    type KeyType = H::HashTypeUnextendable;
    const KEY_BITS: usize = size_of::<H::HashTypeUnextendable>() * 8;

    #[inline(always)]
    fn compare(
        left: &HashEntry<<H as HashFunctionFactory>::HashTypeUnextendable>,
        right: &HashEntry<<H as HashFunctionFactory>::HashTypeUnextendable>,
    ) -> std::cmp::Ordering {
        left.hash.cmp(&right.hash)
    }

    #[inline(always)]
    fn get_shifted(value: &HashEntry<H::HashTypeUnextendable>, rhs: u8) -> u8 {
        H::get_shifted(value.hash, rhs) as u8
    }
}
