use crate::context::Context;
use crate::error::JobSchedulerError;
use crate::job::to_code::{JobCode, NotificationCode};
use crate::job::{JobCreator, JobDeleter, JobLocked, JobRunner};
use crate::notification::{NotificationCreator, NotificationDeleter, NotificationRunner};
use crate::scheduler::{Scheduler, StartResult};
use crate::simple::{
    SimpleJobCode, SimpleMetadataStore, SimpleNotificationCode, SimpleNotificationStore,
};
use crate::store::{MetaDataStorage, NotificationStore};
use chrono::{DateTime, NaiveDateTime, Utc};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(feature = "signal")]
use tokio::signal::unix::SignalKind;
use tokio::sync::RwLock;
use tracing::{error, info};
use uuid::Uuid;

pub type ShutdownNotification =
    dyn FnMut() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync;

/// The JobScheduler contains and executes the scheduled jobs.
pub struct JobsSchedulerLocked {
    pub context: Arc<Context>,
    pub inited: Arc<RwLock<bool>>,
    pub job_creator: Arc<RwLock<JobCreator>>,
    pub job_deleter: Arc<RwLock<JobDeleter>>,
    pub job_runner: Arc<RwLock<JobRunner>>,
    pub notification_creator: Arc<RwLock<NotificationCreator>>,
    pub notification_deleter: Arc<RwLock<NotificationDeleter>>,
    pub notification_runner: Arc<RwLock<NotificationRunner>>,
    pub scheduler: Arc<RwLock<Scheduler>>,
    pub shutdown_notifier: Option<Arc<RwLock<Box<ShutdownNotification>>>>,
}

impl Clone for JobsSchedulerLocked {
    fn clone(&self) -> Self {
        JobsSchedulerLocked {
            context: self.context.clone(),
            inited: self.inited.clone(),
            job_creator: self.job_creator.clone(),
            job_deleter: self.job_deleter.clone(),
            job_runner: self.job_runner.clone(),
            notification_creator: self.notification_creator.clone(),
            notification_deleter: self.notification_deleter.clone(),
            notification_runner: self.notification_runner.clone(),
            scheduler: self.scheduler.clone(),
            shutdown_notifier: self.shutdown_notifier.clone(),
        }
    }
}

impl JobsSchedulerLocked {
    async fn init_context(
        metadata_storage: Arc<RwLock<Box<dyn MetaDataStorage + Send + Sync>>>,
        notification_storage: Arc<RwLock<Box<dyn NotificationStore + Send + Sync>>>,
        job_code: Arc<RwLock<Box<dyn JobCode + Send + Sync>>>,
        notify_code: Arc<RwLock<Box<dyn NotificationCode + Send + Sync>>>,
    ) -> Result<Arc<Context>, JobSchedulerError> {
        {
            let mut metadata_storage = metadata_storage.write().await;
            metadata_storage.init().await?;
        }
        {
            let mut notification_storage = notification_storage.write().await;
            notification_storage.init().await?;
        }
        let context = Context::new(
            metadata_storage,
            notification_storage,
            job_code.clone(),
            notify_code.clone(),
        );
        {
            let mut job_code = job_code.write().await;
            job_code.init(&context).await?;
        }
        {
            let mut notification_code = notify_code.write().await;
            notification_code.init(&context).await?;
        }
        Ok(Arc::new(context))
    }

    async fn init_actors(self) -> Result<(), JobSchedulerError> {
        let for_job_runner = self.clone();
        let Self {
            context,
            job_creator,
            job_deleter,
            job_runner,
            notification_creator,
            notification_deleter,
            notification_runner,
            scheduler,
            ..
        } = self;

        {
            let job_creator = job_creator.write().await;
            job_creator.init(&context).await?;
        }

        {
            let mut job_deleter = job_deleter.write().await;
            job_deleter.init(&context).await?;
        }

        {
            let mut notification_creator = notification_creator.write().await;
            notification_creator.init(&context).await?;
        }

        {
            let mut notification_deleter = notification_deleter.write().await;
            notification_deleter.init(&context).await?;
        }

        {
            let mut notification_runner = notification_runner.write().await;
            notification_runner.init(&context).await?;
        }

        {
            let mut runner = job_runner.write().await;
            runner.init(&context, for_job_runner).await?;
        }

        {
            let mut scheduler = scheduler.write().await;
            scheduler.init(&context);
        }

        Ok(())
    }

    ///
    /// Get whether the scheduler is initialized
    pub async fn inited(&self) -> bool {
        let r = self.inited.read().await;
        *r
    }

    ///
    /// Initialize the actors
    pub async fn init(&mut self) -> Result<(), JobSchedulerError> {
        if self.inited().await {
            return Ok(());
        }
        {
            let mut w = self.inited.write().await;
            *w = true;
        }
        self.clone()
            .init_actors()
            .await
            .map_err(|_| JobSchedulerError::CantInit)
    }

    ///
    /// Create a new `MetaDataStorage` and `NotificationStore` using the `SimpleMetadataStore`, `SimpleNotificationStore`,
    /// `SimpleJobCode` and `SimpleNotificationCode` implementation
    pub async fn new() -> Result<Self, JobSchedulerError> {
        let metadata_storage = SimpleMetadataStore::default();
        let metadata_storage: Arc<RwLock<Box<dyn MetaDataStorage + Send + Sync>>> =
            Arc::new(RwLock::new(Box::new(metadata_storage)));

        let notification_storage = SimpleNotificationStore::default();
        let notification_storage: Arc<RwLock<Box<dyn NotificationStore + Send + Sync>>> =
            Arc::new(RwLock::new(Box::new(notification_storage)));

        let job_code = SimpleJobCode::default();
        let job_code: Arc<RwLock<Box<dyn JobCode + Send + Sync>>> =
            Arc::new(RwLock::new(Box::new(job_code)));

        let notify_code = SimpleNotificationCode::default();
        let notify_code: Arc<RwLock<Box<dyn NotificationCode + Send + Sync>>> =
            Arc::new(RwLock::new(Box::new(notify_code)));

        let context = JobsSchedulerLocked::init_context(
            metadata_storage,
            notification_storage,
            job_code,
            notify_code,
        )
        .await
        .map_err(|_| JobSchedulerError::CantInit)?;

        let val = JobsSchedulerLocked {
            context,
            inited: Arc::new(RwLock::new(false)),
            job_creator: Arc::new(Default::default()),
            job_deleter: Arc::new(Default::default()),
            job_runner: Arc::new(Default::default()),
            notification_creator: Arc::new(Default::default()),
            notification_deleter: Arc::new(Default::default()),
            notification_runner: Arc::new(Default::default()),
            scheduler: Arc::new(Default::default()),
            shutdown_notifier: None,
        };

        Ok(val)
    }

    ///
    /// Create a new `JobsSchedulerLocked` using custom metadata and notification runners, job and notification
    /// code providers
    pub fn new_with_storage_and_code(
        metadata_storage: Box<dyn MetaDataStorage + Send + Sync>,
        notification_storage: Box<dyn NotificationStore + Send + Sync>,
        job_code: Box<dyn JobCode + Send + Sync>,
        notification_code: Box<dyn NotificationCode + Send + Sync>,
    ) -> Result<Self, JobSchedulerError> {
        let metadata_storage = Arc::new(RwLock::new(metadata_storage));
        let notification_storage = Arc::new(RwLock::new(notification_storage));
        let job_code = Arc::new(RwLock::new(job_code));
        let notification_code = Arc::new(RwLock::new(notification_code));

        let (storage_init_tx, storage_init_rx) = std::sync::mpsc::channel();

        tokio::spawn(async move {
            let context = JobsSchedulerLocked::init_context(
                metadata_storage,
                notification_storage,
                job_code,
                notification_code,
            )
            .await;
            if let Err(e) = storage_init_tx.send(context) {
                error!("Error sending init success {:?}", e);
            }
        });

        let context = storage_init_rx
            .recv()
            .map_err(|_| JobSchedulerError::CantInit)??;

        let val = JobsSchedulerLocked {
            context,
            inited: Arc::new(RwLock::new(false)),
            job_creator: Arc::new(Default::default()),
            job_deleter: Arc::new(Default::default()),
            job_runner: Arc::new(Default::default()),
            notification_creator: Arc::new(Default::default()),
            notification_deleter: Arc::new(Default::default()),
            notification_runner: Arc::new(Default::default()),
            scheduler: Arc::new(Default::default()),
            shutdown_notifier: None,
        };

        Ok(val)
    }

    /// Add a job to the `JobScheduler`
    ///
    /// ```rust,ignore
    /// use tokio_cron_scheduler::{Job, JobScheduler, JobToRun};
    /// let mut sched = JobScheduler::new();
    /// sched.add(Job::new("1/10 * * * * *".parse().unwrap(), || {
    ///     println!("I get executed every 10 seconds!");
    /// })).await;
    /// ```
    pub async fn add(&self, job: JobLocked) -> Result<Uuid, JobSchedulerError> {
        let guid = job.guid();
        if !self.inited().await {
            info!("Uninited");
            let mut s = self.clone();
            s.init().await?;
        }

        let context = self.context.clone();
        JobCreator::add(&context, job).await?;
        info!("Job creator created");

        Ok(guid)
    }

    /// Remove a job from the `JobScheduler`
    ///
    /// ```rust,ignore
    /// use tokio_cron_scheduler::{Job, JobScheduler, JobToRun};
    /// let mut sched = JobScheduler::new();
    /// let job_id = sched.add(Job::new("1/10 * * * * *".parse().unwrap(), || {
    ///     println!("I get executed every 10 seconds!");
    /// }))?.await;
    /// sched.remove(job_id).await;
    /// ```
    ///
    /// Note, the UUID of the job can be fetched calling .guid() on a Job.
    ///
    pub async fn remove(&self, to_be_removed: &Uuid) -> Result<(), JobSchedulerError> {
        if !self.inited().await {
            let mut s = self.clone();
            s.init().await?;
        }

        let context = self.context();
        JobDeleter::remove(&context, to_be_removed)
    }

    /// The `tick` method increments time for the JobScheduler and executes
    /// any pending jobs. It is recommended to sleep for at least 500
    /// milliseconds between invocations of this method.
    /// This is kept public if you're running this yourself. It is better to
    /// call the `start` method if you want all of this automated for you.
    ///
    /// ```rust,ignore
    /// loop {
    ///     sched.tick().await;
    ///     std::thread::sleep(Duration::from_millis(500));
    /// }
    /// ```
    pub async fn tick(&self) -> Result<(), JobSchedulerError> {
        if !self.inited().await {
            let mut s = self.clone();
            s.init().await?;
        }
        let ret = self.scheduler.write().await;
        let ret = ret.tick();
        match ret {
            Ok(ret) => Ok(ret),
            Err(e) => {
                error!("Error receiving tick result {:?}", e);
                Err(JobSchedulerError::TickError)
            }
        }
    }

    /// The `start` spawns a Tokio task where it loops. Every 500ms it
    /// runs the tick method to increment any
    /// any pending jobs.
    ///
    /// ```rust,ignore
    /// if let Err(e) = sched.start().await {
    ///         eprintln!("Error on scheduler {:?}", e);
    ///     }
    /// ```
    pub async fn start(&self) -> StartResult {
        if !self.inited().await {
            let mut s = self.clone();
            s.init().await?;
        }
        let mut scheduler = self.scheduler.write().await;
        let ret = scheduler.start();

        match ret {
            Ok(ret) => Ok(ret),
            Err(e) => {
                error!("Error receiving start result {:?}", e);
                Err(JobSchedulerError::StartScheduler)
            }
        }
    }

    /// The `time_till_next_job` method returns the duration till the next job
    /// is supposed to run. This can be used to sleep until then without waking
    /// up at a fixed interval.AsMut
    ///
    /// ```rust, ignore
    /// loop {
    ///     sched.tick().await;
    ///     std::thread::sleep(sched.time_till_next_job());
    /// }
    /// ```
    pub async fn time_till_next_job(
        &mut self,
    ) -> Result<Option<std::time::Duration>, JobSchedulerError> {
        if !self.inited().await {
            let mut s = self.clone();
            s.init().await?;
        }
        let metadata = self.context.metadata_storage.clone();

        let mut metadata = metadata.write().await;
        let ret = metadata.time_till_next_job().await;

        match ret {
            Ok(ret) => Ok(ret),
            Err(e) => {
                error!("Error getting return of time till next job {:?}", e);
                Err(JobSchedulerError::CantGetTimeUntil)
            }
        }
    }

    /// `next_tick_for_job` returns the date/time for when the next tick will
    /// be for a job
    pub async fn next_tick_for_job(
        &mut self,
        job_id: Uuid,
    ) -> Result<Option<DateTime<Utc>>, JobSchedulerError> {
        if !self.inited().await {
            let mut s = self.clone();
            s.init().await?;
        }
        let mut r = self.context.metadata_storage.write().await;
        r.get(job_id).await.map(|v| {
            v.map(|vv| vv.next_tick)
                .filter(|t| *t != 0)
                .map(|ts| NaiveDateTime::from_timestamp(ts as i64, 0))
                .map(|ts| DateTime::from_utc(ts, Utc))
        })
    }

    ///
    /// Shut the scheduler down
    pub async fn shutdown(&mut self) -> Result<(), JobSchedulerError> {
        let mut notify = None;
        std::mem::swap(&mut self.shutdown_notifier, &mut notify);

        let mut scheduler = self.scheduler.write().await;
        scheduler.shutdown().await;

        if let Some(notify) = notify {
            let mut notify = notify.write().await;
            notify().await;
        }
        Ok(())
    }

    ///
    /// Wait for a signal to shut the runtime down with
    #[cfg(feature = "signal")]
    pub fn shutdown_on_signal(&self, signal: SignalKind) {
        let mut l = self.clone();
        tokio::spawn(async move {
            if let Some(_k) = tokio::signal::unix::signal(signal)
                .expect("Can't wait for signal")
                .recv()
                .await
            {
                l.shutdown().await.expect("Problem shutting down");
            }
        });
    }

    ///
    /// Wait for a signal to shut the runtime down with
    #[cfg(feature = "signal")]
    pub fn shutdown_on_ctrl_c(&self) {
        let mut l = self.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c()
                .await
                .expect("Could not await ctrl-c");

            if let Err(err) = l.shutdown().await {
                error!("{:?}", err);
            }
        });
    }

    ///
    /// Code that is run after the shutdown was run
    pub fn set_shutdown_handler(&mut self, job: Box<ShutdownNotification>) {
        self.shutdown_notifier = Some(Arc::new(RwLock::new(job)));
    }

    ///
    /// Remove the shutdown handler
    pub fn remove_shutdown_handler(&mut self) {
        self.shutdown_notifier = None;
    }

    ///
    /// Get the context
    pub fn context(&self) -> Arc<Context> {
        self.context.clone()
    }
}
