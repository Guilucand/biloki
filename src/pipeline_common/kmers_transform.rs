use crate::config::{DEFAULT_OUTPUT_BUFFER_SIZE, DEFAULT_PREFETCH_AMOUNT, MINIMUM_LOG_DELTA_TIME};
use crate::io::concurrent::temp_reads::extra_data::SequenceExtraData;
use crate::KEEP_FILES;
use crossbeam::queue::ArrayQueue;
use parallel_processor::buckets::readers::async_binary_reader::{
    AsyncBinaryReader, AsyncReaderThread,
};
use parallel_processor::buckets::readers::BucketReader;
use parallel_processor::memory_fs::{MemoryFs, RemoveFileMode};
use parallel_processor::phase_times_monitor::PHASES_TIMES_MONITOR;
use parallel_processor::utils::scoped_thread_local::ScopedThreadLocal;
use parking_lot::Mutex;
use std::cmp::min;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub trait KmersTransformExecutorFactory: Sized + 'static + Sync + Send {
    type GlobalExtraData<'a>: Send + Sync + 'a;
    type AssociatedExtraData: SequenceExtraData;
    type ExecutorType<'a>: KmersTransformExecutor<'a, Self>;

    #[allow(non_camel_case_types)]
    type FLAGS_COUNT: typenum::uint::Unsigned;

    fn new<'a>(global_data: &Self::GlobalExtraData<'a>) -> Self::ExecutorType<'a>;
}

pub trait KmersTransformExecutor<'x, F: KmersTransformExecutorFactory> {
    fn process_group<'y: 'x, R: BucketReader>(
        &mut self,
        global_data: &F::GlobalExtraData<'y>,
        reader: R,
    );

    fn finalize<'y: 'x>(self, global_data: &F::GlobalExtraData<'y>);
}

pub struct KmersTransform<'a, F: KmersTransformExecutorFactory> {
    buckets_count: usize,
    processed_buckets: AtomicUsize,

    global_extra_data: F::GlobalExtraData<'a>,
    files_queue: Arc<ArrayQueue<PathBuf>>,
    async_readers: ScopedThreadLocal<Arc<AsyncReaderThread>>,

    last_info_log: Mutex<Instant>,
    _phantom: PhantomData<F>,
}

impl<'a, F: KmersTransformExecutorFactory> KmersTransform<'a, F> {
    pub fn new(
        file_inputs: Vec<PathBuf>,
        buckets_count: usize,
        global_extra_data: F::GlobalExtraData<'a>,
    ) -> Self {
        let files_queue = ArrayQueue::new(file_inputs.len());

        let mut files_with_sizes: Vec<_> = file_inputs
            .into_iter()
            .map(|f| {
                let file_size = MemoryFs::get_file_size(&f).unwrap_or(0);
                (f, file_size)
            })
            .collect();

        files_with_sizes.sort_by_key(|x| x.1);
        files_with_sizes.reverse();

        {
            let mut start_idx = 0;
            let mut end_idx = files_with_sizes.len();

            let mut matched_size = 0i64;

            while start_idx != end_idx {
                let file_entry = if matched_size <= 0 {
                    let target_file = &files_with_sizes[start_idx];
                    let entry = target_file.0.clone();
                    matched_size = target_file.1 as i64;
                    start_idx += 1;
                    entry
                } else {
                    let target_file = &files_with_sizes[end_idx - 1];
                    let entry = target_file.0.clone();
                    matched_size -= target_file.1 as i64;
                    end_idx -= 1;
                    entry
                };

                files_queue.push(file_entry).unwrap();
            }
        }

        Self {
            buckets_count,
            processed_buckets: AtomicUsize::new(0),
            global_extra_data,
            files_queue: Arc::new(files_queue),
            async_readers: ScopedThreadLocal::new(|| {
                AsyncReaderThread::new(DEFAULT_OUTPUT_BUFFER_SIZE / 2, 4)
            }),
            last_info_log: Mutex::new(Instant::now()),
            _phantom: Default::default(),
        }
    }

    fn log_completed_bucket(&self) {
        self.processed_buckets.fetch_add(1, Ordering::Relaxed);

        let mut last_info_log = match self.last_info_log.try_lock() {
            None => return,
            Some(x) => x,
        };
        if last_info_log.elapsed() > MINIMUM_LOG_DELTA_TIME {
            let monitor = PHASES_TIMES_MONITOR.read();

            let processed_count = self.processed_buckets.load(Ordering::Relaxed);
            let remaining = self.buckets_count - processed_count;

            let eta = Duration::from_secs(
                (monitor.get_phase_timer().as_secs_f64() / (processed_count as f64)
                    * (remaining as f64)) as u64,
            );

            let est_tot = Duration::from_secs(
                (monitor.get_phase_timer().as_secs_f64() / (processed_count as f64)
                    * (self.buckets_count as f64)) as u64,
            );

            println!(
                "Processing bucket {} of {} {} phase eta: {:.0?} est.tot: {:.0?}",
                processed_count,
                self.buckets_count,
                monitor.get_formatted_counter_without_memory(),
                eta,
                est_tot
            );
            *last_info_log = Instant::now();
        }
    }

    pub fn parallel_kmers_transform(&self, threads_count: usize) {
        crossbeam::thread::scope(|s| {
            for _ in 0..min(self.buckets_count, threads_count) {
                s.builder()
                    .name("kmers-transform".to_string())
                    .spawn(|_| {
                        let async_reader = self.async_readers.get();
                        let mut executor = F::new(&self.global_extra_data);
                        while let Some(path) = self.files_queue.pop() {
                            let reader = AsyncBinaryReader::new(
                                &path,
                                async_reader.clone(),
                                true,
                                RemoveFileMode::Remove {
                                    remove_fs: !KEEP_FILES.load(Ordering::Relaxed),
                                },
                                DEFAULT_PREFETCH_AMOUNT,
                            );
                            executor.process_group(&self.global_extra_data, reader);
                            self.log_completed_bucket();
                        }
                        executor.finalize(&self.global_extra_data);
                    })
                    .unwrap();
            }
        })
        .unwrap();
    }
}
