// Copyright (c) 2019-present Dmitry Stepanov and Fyrox Engine contributors.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! Resource manager controls loading and lifetime of resource in the engine. See [`ResourceManager`]
//! docs for more info.

use crate::{
    constructor::ResourceConstructorContainer,
    core::{
        append_extension,
        futures::future::join_all,
        io::FileError,
        log::Log,
        make_relative_path, notify,
        parking_lot::{Mutex, MutexGuard},
        task::TaskPool,
        watcher::FileSystemWatcher,
    },
    entry::{TimedEntry, DEFAULT_RESOURCE_LIFETIME},
    event::{ResourceEvent, ResourceEventBroadcaster},
    io::{FsResourceIo, ResourceIo},
    loader::ResourceLoadersContainer,
    metadata::ResourceMetadata,
    options::OPTIONS_EXTENSION,
    registry::{RegistryContainer, RegistryContainerExt, ResourceRegistry},
    state::{LoadError, ResourcePath, ResourceState},
    untyped::ResourceKind,
    Resource, ResourceData, TypedResourceData, UntypedResource,
};
use fxhash::FxHashMap;
use fyrox_core::{err, info, Uuid};
use std::{
    borrow::Cow,
    fmt::{Debug, Display, Formatter},
    marker::PhantomData,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::Arc,
};

/// A set of resources that can be waited for.
#[must_use]
#[derive(Default)]
pub struct ResourceWaitContext {
    resources: Vec<UntypedResource>,
}

impl ResourceWaitContext {
    /// Wait until all resources are loaded (or failed to load).
    #[must_use]
    pub fn is_all_loaded(&self) -> bool {
        let mut loaded_count = 0;
        for resource in self.resources.iter() {
            if !matches!(resource.0.lock().state, ResourceState::Pending { .. }) {
                loaded_count += 1;
            }
        }
        loaded_count == self.resources.len()
    }
}

/// Data source of a built-in resource.
#[derive(Clone)]
pub struct DataSource {
    /// File extension, associated with the data source.
    pub extension: Cow<'static, str>,
    /// The actual data.
    pub bytes: Cow<'static, [u8]>,
}

impl DataSource {
    pub fn new(path: &'static str, data: &'static [u8]) -> Self {
        Self {
            extension: Cow::Borrowed(
                Path::new(path)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or(""),
            ),
            bytes: Cow::Borrowed(data),
        }
    }
}

#[macro_export]
macro_rules! embedded_data_source {
    ($path:expr) => {
        $crate::manager::DataSource::new($path, include_bytes!($path))
    };
}

#[derive(Clone)]
pub struct UntypedBuiltInResource {
    /// Initial data, from which the resource is created from.
    pub data_source: Option<DataSource>,
    /// Ready-to-use ("loaded") resource.
    pub resource: UntypedResource,
}

pub struct BuiltInResource<T>
where
    T: TypedResourceData,
{
    pub id: PathBuf,
    /// Initial data, from which the resource is created from.
    pub data_source: Option<DataSource>,
    /// Ready-to-use ("loaded") resource.
    pub resource: Resource<T>,
}

impl<T: TypedResourceData> Clone for BuiltInResource<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            data_source: self.data_source.clone(),
            resource: self.resource.clone(),
        }
    }
}

impl<T: TypedResourceData> BuiltInResource<T> {
    pub fn new<F>(id: impl AsRef<Path>, data_source: DataSource, make: F) -> Self
    where
        F: FnOnce(&[u8]) -> Resource<T>,
    {
        let resource = make(&data_source.bytes);
        Self {
            id: id.as_ref().to_path_buf(),
            resource,
            data_source: Some(data_source),
        }
    }

    pub fn new_no_source(id: impl AsRef<Path>, resource: Resource<T>) -> Self {
        Self {
            id: id.as_ref().to_path_buf(),
            data_source: None,
            resource,
        }
    }

    pub fn resource(&self) -> Resource<T> {
        self.resource.clone()
    }
}

impl<T: TypedResourceData> From<BuiltInResource<T>> for UntypedBuiltInResource {
    fn from(value: BuiltInResource<T>) -> Self {
        Self {
            data_source: value.data_source,
            resource: value.resource.into(),
        }
    }
}

#[derive(Default, Clone)]
pub struct BuiltInResourcesContainer {
    inner: FxHashMap<PathBuf, UntypedBuiltInResource>,
}

impl BuiltInResourcesContainer {
    pub fn add<T>(&mut self, resource: BuiltInResource<T>)
    where
        T: TypedResourceData,
    {
        self.add_untyped(resource.id.clone(), resource.into())
    }

    pub fn add_untyped(&mut self, id: PathBuf, resource: UntypedBuiltInResource) {
        self.inner.insert(id, resource);
    }
}

impl Deref for BuiltInResourcesContainer {
    type Target = FxHashMap<PathBuf, UntypedBuiltInResource>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for BuiltInResourcesContainer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Internal state of the resource manager.
pub struct ResourceManagerState {
    /// A set of resource loaders. Use this field to register your own resource loader.
    pub loaders: Arc<Mutex<ResourceLoadersContainer>>,
    /// Event broadcaster can be used to "subscribe" for events happening inside the container.
    pub event_broadcaster: ResourceEventBroadcaster,
    /// A container for resource constructors.
    pub constructors_container: ResourceConstructorContainer,
    /// A set of built-in resources, that will be used to resolve references on deserialization.
    pub built_in_resources: BuiltInResourcesContainer,
    /// File system abstraction interface. Could be used to support virtual file systems.
    pub resource_io: Arc<dyn ResourceIo>,
    /// Resource registry, contains associations `UUID -> File Path`. Any access to the registry
    /// must be async, use task pool for this.
    pub resource_registry: Arc<Mutex<ResourceRegistry>>,

    resources: Vec<TimedEntry<UntypedResource>>,
    task_pool: Arc<TaskPool>,
    watcher: Option<FileSystemWatcher>,
}

/// Resource manager controls loading and lifetime of resource in the engine. Resource manager can hold
/// resources of arbitrary types via type erasure mechanism.
///
/// ## Built-in Resources
///
/// Built-in resources are special kinds of resources, whose data is packed in the executable (i.e. via
/// [`include_bytes`] macro). Such resources reference the data that cannot be "loaded" from external
/// source. To support such kind of resource the manager provides `built_in_resources` hash map where
/// you can register your own built-in resource and access existing ones.
///
/// ## Internals
///
/// It is a simple wrapper over [`ResourceManagerState`] that can be shared (cloned). In other words,
/// it is just a strong reference to the inner state.
#[derive(Clone)]
pub struct ResourceManager {
    state: Arc<Mutex<ResourceManagerState>>,
}

impl Debug for ResourceManager {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ResourceManager")
    }
}

/// An error that may occur during texture registration.
#[derive(Debug)]
pub enum ResourceRegistrationError {
    /// Resource saving has failed.
    UnableToRegister,
    /// Resource was in invalid state (Pending, LoadErr)
    InvalidState,
    /// Resource is already registered.
    AlreadyRegistered,
}

impl Display for ResourceRegistrationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ResourceRegistrationError::UnableToRegister => {
                write!(f, "Unable to register the resource!")
            }
            ResourceRegistrationError::InvalidState => {
                write!(f, "A resource was in invalid state!")
            }
            ResourceRegistrationError::AlreadyRegistered => {
                write!(f, "A resource is already registered!")
            }
        }
    }
}

impl ResourceManager {
    /// Creates a resource manager with default settings and loaders.
    pub fn new(task_pool: Arc<TaskPool>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ResourceManagerState::new(task_pool))),
        }
    }

    /// Returns a guarded reference to internal state of resource manager.
    pub fn state(&self) -> MutexGuard<'_, ResourceManagerState> {
        self.state.lock()
    }

    /// Returns the ResourceIo used by this resource manager
    pub fn resource_io(&self) -> Arc<dyn ResourceIo> {
        let state = self.state();
        state.resource_io.clone()
    }

    /// Returns the task pool used by this resource manager.
    pub fn task_pool(&self) -> Arc<TaskPool> {
        let state = self.state();
        state.task_pool()
    }

    /// Requests a resource of the given type located at the given path. This method is non-blocking, instead
    /// it immediately returns the typed resource wrapper. Loading of the resource is managed automatically in
    /// a separate thread (or thread pool) on PC, and JS micro-task (the same thread) on WebAssembly.
    ///
    /// ## Type Guarantees
    ///
    /// There's no strict guarantees that the requested resource will be of the requested type. This
    /// is because the resource system is fully async and does not have access to type information in
    /// most cases. Initial type checking is not very reliable and can be "fooled" pretty easily,
    /// simply because it just checks if there's a registered loader for a specific extension.
    ///
    /// ## Sharing
    ///
    /// If the resource at the given path is already was requested (no matter in which state the actual resource
    /// is), this method will return the existing instance. This way the resource manager guarantees that the actual
    /// resource data will be loaded once, and it can be shared.
    ///
    /// ## Waiting
    ///
    /// If you need to wait until the resource is loaded, use `.await` on the result of the method. Every resource
    /// implements `Future` trait and can be used in `async` contexts.
    ///
    /// ## Resource state
    ///
    /// Keep in mind, that the resource itself is a small state machine. It could be in three main states:
    ///
    /// - [`ResourceState::Pending`] - a resource is in the queue to load or still loading.
    /// - [`ResourceState::LoadError`] - a resource is failed to load.
    /// - [`ResourceState::Ok`] - a resource is successfully loaded.
    ///
    /// Actual resource state can be fetched by [`Resource::state`] method. If you know for sure that the resource
    /// is already loaded, then you can use [`Resource::data_ref`] to obtain a reference to the actual resource data.
    /// Keep in mind, that this method will panic if the resource non in `Ok` state.
    ///
    /// ## Panic
    ///
    /// This method will panic, if type UUID of `T` does not match the actual type UUID of the resource. If this
    /// is undesirable, use [`Self::try_request`] instead.
    pub fn request<T>(&self, path: impl AsRef<Path>) -> Resource<T>
    where
        T: TypedResourceData,
    {
        let mut state = self.state();

        assert!(state
            .loaders
            .lock()
            .is_extension_matches_type::<T>(path.as_ref()));

        Resource {
            untyped: state.request(path),
            phantom: PhantomData::<T>,
        }
    }

    /// The same as [`Self::request`], but returns [`None`] if type UUID of `T` does not match the actual type UUID
    /// of the resource.
    ///
    /// ## Panic
    ///
    /// This method does not panic.
    pub fn try_request<T>(&self, path: impl AsRef<Path>) -> Option<Resource<T>>
    where
        T: TypedResourceData,
    {
        let mut state = self.state();
        let untyped = state.request(path.as_ref());
        if state
            .loaders
            .lock()
            .is_extension_matches_type::<T>(path.as_ref())
        {
            Some(Resource {
                untyped,
                phantom: PhantomData::<T>,
            })
        } else {
            None
        }
    }

    pub fn resource_path(&self, resource: &UntypedResource) -> Option<PathBuf> {
        self.state().resource_path(resource)
    }

    /// Same as [`Self::request`], but returns untyped resource.
    pub fn request_untyped<P>(&self, path: P) -> UntypedResource
    where
        P: AsRef<Path>,
    {
        self.state().request(path)
    }

    pub fn request_by_uuid(&self, resource_uuid: Uuid) -> UntypedResource {
        self.state().request_by_uuid(resource_uuid)
    }

    /// Saves given resources in the specified path and registers it in resource manager, so
    /// it will be accessible through it later.
    pub fn register<P, F>(
        &self,
        resource: UntypedResource,
        path: P,
        mut on_register: F,
    ) -> Result<(), ResourceRegistrationError>
    where
        P: AsRef<Path>,
        F: FnMut(&mut dyn ResourceData, &Path) -> bool,
    {
        let path = path.as_ref().to_owned();
        let resource_uuid = resource
            .resource_uuid()
            .ok_or(ResourceRegistrationError::InvalidState)?;

        let mut state = self.state();
        if let Some(resource) = state.find(resource_uuid) {
            let resource_state = resource.0.lock();
            if let ResourceState::Ok { .. } = resource_state.state {
                return Err(ResourceRegistrationError::AlreadyRegistered);
            }
        }

        state.unregister(&path);

        let mut header = resource.0.lock();
        header.kind.make_external();
        if let ResourceState::Ok {
            ref mut data,
            resource_uuid,
            ..
        } = header.state
        {
            if !on_register(&mut **data, path.as_ref()) {
                Err(ResourceRegistrationError::UnableToRegister)
            } else {
                state.resource_registry.lock().register(resource_uuid, path);
                drop(header);
                state.push(resource);
                Ok(())
            }
        } else {
            Err(ResourceRegistrationError::InvalidState)
        }
    }

    /// Attempts to move a resource from its current location to the new path.
    pub async fn move_resource(
        &self,
        resource: UntypedResource,
        new_path: impl AsRef<Path>,
    ) -> Result<(), FileError> {
        let resource_uuid = resource
            .resource_uuid()
            .ok_or_else(|| FileError::Custom("Unable to move non-loaded resource!".to_string()))?;

        let new_path = new_path.as_ref().to_owned();
        let io = self.state().resource_io.clone();
        let registry = self.state().resource_registry.clone();
        let existing_path = registry
            .lock()
            .uuid_to_path(resource_uuid)
            .map(|path| path.to_path_buf())
            .ok_or_else(|| FileError::Custom("Cannot move embedded resource!".to_string()))?;

        // Move the file with its optional import options and mandatory metadata.
        io.move_file(&existing_path, &new_path).await?;

        let options_path = append_extension(&existing_path, OPTIONS_EXTENSION);
        if io.exists(&options_path).await {
            let new_options_path = append_extension(&new_path, OPTIONS_EXTENSION);
            io.move_file(&options_path, &new_options_path).await?;
        }

        let metadata_path = append_extension(&existing_path, ResourceMetadata::EXTENSION);
        if io.exists(&metadata_path).await {
            let new_metadata_path = append_extension(&new_path, ResourceMetadata::EXTENSION);
            io.move_file(&metadata_path, &new_metadata_path).await?;
        }

        Ok(())
    }

    /// Reloads all loaded resources. Normally it should never be called, because it is **very** heavy
    /// method! This method is asynchronous, it uses all available CPU power to reload resources as
    /// fast as possible.
    pub async fn reload_resources(&self) {
        let resources = self.state().reload_resources();
        join_all(resources).await;
    }
}

impl ResourceManagerState {
    pub(crate) fn new(task_pool: Arc<TaskPool>) -> Self {
        Self {
            resources: Default::default(),
            task_pool,
            loaders: Default::default(),
            event_broadcaster: Default::default(),
            constructors_container: Default::default(),
            watcher: None,
            built_in_resources: Default::default(),
            // Use the file system resource io by default
            resource_io: Arc::new(FsResourceIo),
            resource_registry: Arc::new(Mutex::new(ResourceRegistry::default())),
        }
    }

    pub fn request_load_registry(&self, path: PathBuf) {
        info!(
            "Trying to load or update the registry at {}...",
            path.display()
        );

        let task_resource_io = self.resource_io.clone();
        let task_resource_registry = self.resource_registry.clone();
        let is_ready_flag = task_resource_registry.lock().is_ready.clone();
        is_ready_flag.mark_as_not_ready();
        let task_loaders = self.loaders.clone();
        self.task_pool.spawn_task(async move {
            match RegistryContainer::load_from_file(&path, &*task_resource_io).await {
                Ok(registry) => {
                    let mut lock = task_resource_registry.lock();
                    lock.set_container(registry);

                    is_ready_flag.mark_as_ready();

                    info!(
                        "Resource registry was loaded from {} successfully!",
                        path.display()
                    );
                }
                Err(error) => {
                    err!(
                        "Unable to load resource registry! Reason: {:?}. \
                    Trying to update the registry!",
                        error
                    );

                    let new_data =
                        ResourceRegistry::scan(task_resource_io.clone(), task_loaders, &path).await;
                    if let Err(error) = new_data.save(&path, &*task_resource_io).await {
                        err!(
                            "Unable to write the resource registry at the {} path! Reason: {:?}",
                            path.display(),
                            error
                        )
                    }
                    let mut lock = task_resource_registry.lock();
                    lock.set_container(new_data);
                    lock.is_ready.mark_as_ready();

                    info!(
                        "Resource registry was updated and written to {} successfully!",
                        path.display()
                    );
                }
            };
        });
    }

    /// Returns the task pool used by this resource manager.
    pub fn task_pool(&self) -> Arc<TaskPool> {
        self.task_pool.clone()
    }

    /// Set the IO source that the resource manager should use when
    /// loading assets
    pub fn set_resource_io(&mut self, resource_io: Arc<dyn ResourceIo>) {
        self.resource_io = resource_io;
    }

    /// Sets resource watcher which will track any modifications in file system and forcing
    /// the manager to reload changed resources. By default there is no watcher, since it
    /// may be an undesired effect to reload resources at runtime. This is very useful thing
    /// for fast iterative development.
    pub fn set_watcher(&mut self, watcher: Option<FileSystemWatcher>) {
        self.watcher = watcher;
    }

    /// Returns total amount of registered resources.
    pub fn count_registered_resources(&self) -> usize {
        self.resources.len()
    }

    /// Returns percentage of loading progress. This method is useful to show progress on
    /// loading screen in your game. This method could be used alone if your game depends
    /// only on external resources, or if your game doing some heavy calculations this value
    /// can be combined with progress of your tasks.
    pub fn loading_progress(&self) -> usize {
        let registered = self.count_registered_resources();
        if registered > 0 {
            self.count_loaded_resources() * 100 / registered
        } else {
            100
        }
    }

    pub fn update_registry(&mut self) {
        let io = self.resource_io.clone();
        let loaders = self.loaders.clone();
        let registry = self.resource_registry.clone();
        registry.lock().is_ready.mark_as_not_ready();
        self.task_pool.spawn_task(async move {
            let path = ResourceRegistry::DEFAULT_PATH;
            let new_data = ResourceRegistry::scan(io.clone(), loaders, path).await;
            if let Err(error) = new_data.save(Path::new(path), &*io).await {
                err!(
                    "Unable to write the resource registry at the {} path! Reason: {:?}",
                    path,
                    error
                )
            }
            let mut lock = registry.lock();
            lock.set_container(new_data);
            lock.is_ready.mark_as_ready();
        });
    }

    /// Update resource containers and do hot-reloading.
    ///
    /// Resources are removed if they're not used
    /// or reloaded if they have changed in disk.
    ///
    /// Normally, this is called from `Engine::update()`.
    /// You should only call this manually if you don't use that method.
    pub fn update(&mut self, dt: f32) {
        self.resources.retain_mut(|resource| {
            // One usage means that the resource has single owner, and that owner
            // is this container. Such resources have limited life time, if the time
            // runs out before it gets shared again, the resource will be deleted.
            if resource.value.use_count() <= 1 {
                resource.time_to_live -= dt;
                if resource.time_to_live <= 0.0 {
                    let registry = self.resource_registry.lock();
                    let resource_uuid = resource.resource_uuid();
                    if let Some(path) =
                        resource_uuid.and_then(|resource_uuid| registry.uuid_to_path(resource_uuid))
                    {
                        Log::info(format!(
                            "Resource {} destroyed because it is not used anymore!",
                            path.display()
                        ));

                        self.event_broadcaster
                            .broadcast(ResourceEvent::Removed(path.to_path_buf()));
                    }

                    false
                } else {
                    // Keep resource alive for short period of time.
                    true
                }
            } else {
                // Make sure to reset timer if a resource is used by more than one owner.
                resource.time_to_live = DEFAULT_RESOURCE_LIFETIME;

                // Keep resource alive while it has more than one owner.
                true
            }
        });

        if let Some(watcher) = self.watcher.as_ref() {
            if let Some(evt) = watcher.try_get_event() {
                if let notify::EventKind::Modify(_) = evt.kind {
                    for path in evt.paths {
                        if let Ok(relative_path) = make_relative_path(path) {
                            if self.try_reload_resource_from_path(&relative_path) {
                                Log::info(format!(
                                    "File {} was changed, trying to reload a respective resource...",
                                    relative_path.display()
                                ));

                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Adds a new resource in the container.
    pub fn push(&mut self, resource: UntypedResource) {
        self.event_broadcaster
            .broadcast(ResourceEvent::Added(resource.clone()));

        self.resources.push(TimedEntry {
            value: resource,
            time_to_live: DEFAULT_RESOURCE_LIFETIME,
        });
    }

    /// Tries to find a resource by its path. Returns None if no resource was found.
    ///
    /// # Complexity
    ///
    /// O(n)
    pub fn find(&self, uuid: Uuid) -> Option<&UntypedResource> {
        self.resources
            .iter()
            .find(|entry| entry.value.resource_uuid() == Some(uuid))
            .map(|entry| &entry.value)
    }

    pub fn find_by_path(&self, path: &Path) -> Option<&UntypedResource> {
        let registry = self.resource_registry.lock();
        self.resources.iter().find_map(|entry| {
            let header = entry.value.0.lock();
            if let ResourceState::Ok { resource_uuid, .. } = header.state {
                if registry.uuid_to_path(resource_uuid) == Some(path) {
                    return Some(&entry.value);
                }
            }
            None
        })
    }

    /// Returns total amount of resources in the container.
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    /// Returns true if the resource manager has no resources.
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// Creates an iterator over resources in the manager.
    pub fn iter(&self) -> impl Iterator<Item = &UntypedResource> {
        self.resources.iter().map(|entry| &entry.value)
    }

    /// Immediately destroys all resources in the manager that are not used anywhere else.
    pub fn destroy_unused_resources(&mut self) {
        self.resources
            .retain(|resource| resource.value.use_count() > 1);
    }

    /// Returns total amount of resources that still loading.
    pub fn count_pending_resources(&self) -> usize {
        self.resources.iter().fold(0, |counter, resource| {
            if let ResourceState::Pending { .. } = resource.0.lock().state {
                counter + 1
            } else {
                counter
            }
        })
    }

    /// Returns total amount of completely loaded resources.
    pub fn count_loaded_resources(&self) -> usize {
        self.resources.iter().fold(0, |counter, resource| {
            if let ResourceState::Ok { .. } = resource.0.lock().state {
                counter + 1
            } else {
                counter
            }
        })
    }

    /// Returns a set of resource handled by this container.
    pub fn resources(&self) -> Vec<UntypedResource> {
        self.resources.iter().map(|t| t.value.clone()).collect()
    }

    /// Tries to load a resources at a given path.
    pub fn request<P>(&mut self, path: P) -> UntypedResource
    where
        P: AsRef<Path>,
    {
        if let Some(built_in_resource) = self.built_in_resources.get(path.as_ref()) {
            return built_in_resource.resource.clone();
        }

        self.find_or_load(ResourcePath::Explicit(path.as_ref().to_path_buf()))
    }

    /// Tries to load a resource by a unique identifier. The identifier must not be a zero-uuid,
    /// otherwise this method will panic!
    pub fn request_by_uuid(&mut self, resource_uuid: Uuid) -> UntypedResource {
        if let Some(built_in_resource) = self
            .built_in_resources
            .values()
            .find(|r| r.resource.resource_uuid() == Some(resource_uuid))
        {
            return built_in_resource.resource.clone();
        }

        self.find_or_load(ResourcePath::Implicit(resource_uuid))
    }

    fn find_by_resource_path(&self, path_to_search: &ResourcePath) -> Option<&UntypedResource> {
        self.resources
            .iter()
            .find(|entry| {
                let header = entry.value.0.lock();
                match header.state {
                    ResourceState::Pending { ref path, .. }
                    | ResourceState::LoadError { ref path, .. } => path == path_to_search,
                    ResourceState::Ok { resource_uuid, .. } => match path_to_search {
                        ResourcePath::Explicit(fs_path) => {
                            self.resource_registry.lock().uuid_to_path(resource_uuid)
                                == Some(fs_path)
                        }
                        ResourcePath::Implicit(uuid) => &resource_uuid == uuid,
                    },
                }
            })
            .map(|entry| &entry.value)
    }

    fn find_or_load(&mut self, path: ResourcePath) -> UntypedResource {
        match self.find_by_resource_path(&path) {
            Some(existing) => existing.clone(),
            None => {
                let resource = UntypedResource::new_pending(ResourceKind::External);
                self.spawn_loading_task(path, resource.clone(), false);
                self.push(resource.clone());
                resource
            }
        }
    }

    fn spawn_loading_task(&self, path: ResourcePath, resource: UntypedResource, reload: bool) {
        if let ResourcePath::Implicit(ref uuid) = path {
            assert_ne!(*uuid, Uuid::nil());
        }

        let event_broadcaster = self.event_broadcaster.clone();
        let loaders = self.loaders.clone();
        let registry = self.resource_registry.clone();
        let io = self.resource_io.clone();
        let is_registry_ready = registry.lock().is_ready.clone();

        self.task_pool.spawn_task(async move {
            // Wait until the registry is fully loaded.
            is_registry_ready.await;

            // A resource can be requested either by a path or an uuid. We need the registry
            // to find a respective path for an uuid.
            let fs_path = match path {
                ResourcePath::Explicit(ref path) => path.clone(),
                ResourcePath::Implicit(uuid) => {
                    if let Some(path) = registry.lock().uuid_to_path(uuid).map(|p| p.to_path_buf())
                    {
                        path
                    } else {
                        resource.commit_error(
                            path,
                            LoadError::new(format!(
                                "Unable to load a resource by {uuid} id! There's no matching \
                            path to it in the resource registry. A resource might be deleted \
                            or the registry is outdated.",
                            )),
                        );

                        return;
                    }
                }
            };

            // Try to find a loader for the resource.
            let loader_future = loaders
                .lock()
                .loader_for(&fs_path)
                .map(|loader| loader.load(fs_path.clone(), io));

            if let Some(loader_future) = loader_future {
                match loader_future.await {
                    Ok(data) => {
                        let data = data.0;

                        Log::info(format!(
                            "Resource {} was loaded successfully!",
                            fs_path.display()
                        ));

                        // Separate scope to keep mutex locking time at minimum.
                        {
                            let mut mutex_guard = resource.0.lock();
                            let resource_uuid = registry.lock().path_to_uuid(&fs_path).unwrap();
                            assert!(mutex_guard.kind.is_external());
                            mutex_guard.state.commit(ResourceState::Ok {
                                data,
                                resource_uuid,
                            });
                        }

                        event_broadcaster.broadcast_loaded_or_reloaded(resource, reload);
                    }
                    Err(error) => {
                        Log::info(format!(
                            "Resource {} failed to load. Reason: {:?}",
                            fs_path.display(),
                            error
                        ));

                        resource.commit_error(path, error);
                    }
                }
            } else {
                resource.commit_error(
                    path,
                    LoadError::new(format!(
                        "There's no resource loader for {} resource!",
                        fs_path.display()
                    )),
                )
            }
        });
    }

    pub fn resource_path(&self, resource: &UntypedResource) -> Option<PathBuf> {
        let header = resource.0.lock();
        if let ResourceState::Ok { resource_uuid, .. } = header.state {
            let registry = self.resource_registry.lock();
            registry.uuid_to_path_buf(resource_uuid)
        } else {
            None
        }
    }

    /// Reloads a single resource.
    pub fn reload_resource(&mut self, resource: UntypedResource) {
        let header = resource.0.lock();
        match header.state {
            ResourceState::Pending { .. } => {
                // The resource is loading already.
            }
            ResourceState::LoadError { ref path, .. } => {
                let path = path.clone();
                drop(header);
                self.spawn_loading_task(path, resource, true)
            }
            ResourceState::Ok { resource_uuid, .. } => {
                let path = ResourcePath::Implicit(resource_uuid);
                drop(header);
                self.spawn_loading_task(path, resource, true)
            }
        }
    }

    /// Reloads all resources in the container. Returns a list of resources that will be reloaded.
    /// You can use the list to wait until all resources are loading.
    pub fn reload_resources(&mut self) -> Vec<UntypedResource> {
        let resources = self
            .resources
            .iter()
            .map(|r| r.value.clone())
            .collect::<Vec<_>>();

        for resource in resources.iter().cloned() {
            self.reload_resource(resource);
        }

        resources
    }

    /// Wait until all resources are loaded (or failed to load).
    pub fn get_wait_context(&self) -> ResourceWaitContext {
        ResourceWaitContext {
            resources: self
                .resources
                .iter()
                .map(|e| e.value.clone())
                .collect::<Vec<_>>(),
        }
    }

    /// Tries to reload a resource at the given path.
    pub fn try_reload_resource_from_path(&mut self, path: &Path) -> bool {
        let mut registry = self.resource_registry.lock();
        if let Some(uuid) = registry.unregister_path(path) {
            drop(registry);
            if let Some(resource) = self.find(uuid).cloned() {
                self.reload_resource(resource);
                return true;
            }
        }
        false
    }

    /// Forgets that a resource at the given path was ever loaded, thus making it possible to reload it
    /// again as a new instance.
    pub fn unregister(&mut self, path: &Path) {
        if let Some(uuid) = self.resource_registry.lock().unregister_path(path) {
            if let Some(position) = self
                .resources
                .iter()
                .position(|entry| entry.value.resource_uuid() == Some(uuid))
            {
                self.resources.remove(position);
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::error::Error;
    use std::{fs::File, time::Duration};

    use crate::loader::{BoxedLoaderFuture, LoaderPayload, ResourceLoader};

    use super::*;

    use fyrox_core::uuid::{uuid, Uuid};
    use fyrox_core::{
        reflect::prelude::*,
        visitor::{Visit, VisitResult, Visitor},
        TypeUuidProvider,
    };

    #[derive(Debug, Default, Reflect, Visit)]
    struct Stub {}

    impl TypeUuidProvider for Stub {
        fn type_uuid() -> Uuid {
            uuid!("9d873ff4-3126-47e1-a492-7cd8e7168239")
        }
    }

    impl ResourceData for Stub {
        fn type_uuid(&self) -> Uuid {
            <Self as TypeUuidProvider>::type_uuid()
        }

        fn save(&mut self, _path: &Path) -> Result<(), Box<dyn Error>> {
            Err("Saving is not supported!".to_string().into())
        }

        fn can_be_saved(&self) -> bool {
            false
        }
    }

    impl ResourceLoader for Stub {
        fn extensions(&self) -> &[&str] {
            &["txt"]
        }

        fn data_type_uuid(&self) -> Uuid {
            <Stub as TypeUuidProvider>::type_uuid()
        }

        fn load(&self, _path: PathBuf, _io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
            Box::pin(async move { Ok(LoaderPayload::new(Stub::default())) })
        }
    }

    fn new_resource_manager() -> ResourceManagerState {
        ResourceManagerState::new(Arc::new(Default::default()))
    }

    #[test]
    fn resource_wait_context_is_all_loaded() {
        assert!(ResourceWaitContext::default().is_all_loaded());

        let cx = ResourceWaitContext {
            resources: vec![
                UntypedResource::new_pending(ResourceKind::External),
                UntypedResource::new_load_error(ResourceKind::External, Default::default()),
            ],
        };
        assert!(!cx.is_all_loaded());
    }

    #[test]
    fn resource_manager_state_new() {
        let state = new_resource_manager();

        assert!(state.resources.is_empty());
        assert!(state.loaders.lock().is_empty());
        assert!(state.built_in_resources.is_empty());
        assert!(state.constructors_container.is_empty());
        assert!(state.watcher.is_none());
        assert!(state.is_empty());
    }

    #[test]
    fn resource_manager_state_set_watcher() {
        let mut state = new_resource_manager();
        assert!(state.watcher.is_none());

        let path = PathBuf::from("test.txt");
        if File::create(path.clone()).is_ok() {
            let watcher = FileSystemWatcher::new(path.clone(), Duration::from_secs(1));
            state.set_watcher(watcher.ok());
            assert!(state.watcher.is_some());
        }
    }

    #[test]
    fn resource_manager_state_push() {
        let mut state = new_resource_manager();

        assert_eq!(state.count_loaded_resources(), 0);
        assert_eq!(state.count_pending_resources(), 0);
        assert_eq!(state.count_registered_resources(), 0);
        assert_eq!(state.len(), 0);

        state.push(UntypedResource::new_pending(ResourceKind::External));
        state.push(UntypedResource::new_load_error(
            ResourceKind::External,
            Default::default(),
        ));
        state.push(UntypedResource::new_ok(
            Uuid::new_v4(),
            Default::default(),
            Stub {},
        ));

        assert_eq!(state.count_loaded_resources(), 1);
        assert_eq!(state.count_pending_resources(), 1);
        assert_eq!(state.count_registered_resources(), 3);
        assert_eq!(state.len(), 3);
    }

    #[test]
    fn resource_manager_state_loading_progress() {
        let mut state = new_resource_manager();

        assert_eq!(state.loading_progress(), 100);

        state.push(UntypedResource::new_pending(ResourceKind::External));
        state.push(UntypedResource::new_load_error(
            ResourceKind::External,
            Default::default(),
        ));
        state.push(UntypedResource::new_ok(
            Uuid::new_v4(),
            Default::default(),
            Stub {},
        ));

        assert_eq!(state.loading_progress(), 33);
    }

    #[test]
    fn resource_manager_state_find() {
        let mut state = new_resource_manager();

        assert!(state.find_by_path(Path::new("foo.txt")).is_none());

        let path = PathBuf::from("test.txt");
        let resource = UntypedResource::new_pending(ResourceKind::External);
        state.push(resource.clone());

        assert_eq!(state.find_by_path(&path), Some(&resource));
    }

    #[test]
    fn resource_manager_state_resources() {
        let mut state = new_resource_manager();

        assert_eq!(state.resources(), Vec::new());

        let r1 = UntypedResource::new_pending(ResourceKind::External);
        let r2 = UntypedResource::new_load_error(ResourceKind::External, Default::default());
        let r3 = UntypedResource::new_ok(Uuid::new_v4(), ResourceKind::Embedded, Stub {});
        state.push(r1.clone());
        state.push(r2.clone());
        state.push(r3.clone());

        assert_eq!(state.resources(), vec![r1.clone(), r2.clone(), r3.clone()]);
        assert!(state.iter().eq([&r1, &r2, &r3]));
    }

    #[test]
    fn resource_manager_state_destroy_unused_resources() {
        let mut state = new_resource_manager();

        state.push(UntypedResource::new_pending(ResourceKind::External));
        assert_eq!(state.len(), 1);

        state.destroy_unused_resources();
        assert_eq!(state.len(), 0);
    }

    #[test]
    fn resource_manager_state_request() {
        let mut state = new_resource_manager();
        let path = PathBuf::from("test.txt");

        let resource = UntypedResource::new_load_error(ResourceKind::External, Default::default());
        state.push(resource.clone());

        let res = state.request(path);
        assert_eq!(res, resource);

        let path = PathBuf::from("foo.txt");
        let res = state.request(path.clone());

        assert_eq!(res.kind(), ResourceKind::External);
        assert!(!res.is_loading());
    }

    #[test]
    fn resource_manager_state_try_reload_resource_from_path() {
        let mut state = new_resource_manager();
        state.loaders.lock().set(Stub {});

        let resource = UntypedResource::new_load_error(ResourceKind::External, Default::default());
        state.push(resource.clone());

        assert!(!state.try_reload_resource_from_path(Path::new("foo.txt")));

        assert!(state.try_reload_resource_from_path(Path::new("test.txt")));
        assert!(resource.is_loading());
    }

    #[test]
    fn resource_manager_state_get_wait_context() {
        let mut state = new_resource_manager();

        let resource = UntypedResource::new_ok(Uuid::new_v4(), ResourceKind::External, Stub {});
        state.push(resource.clone());
        let cx = state.get_wait_context();

        assert!(cx.resources.eq(&vec![resource]));
    }

    #[test]
    fn resource_manager_new() {
        let manager = ResourceManager::new(Arc::new(Default::default()));

        assert!(manager.state.lock().is_empty());
        assert!(manager.state().is_empty());
    }

    #[test]
    fn resource_manager_register() {
        let manager = ResourceManager::new(Arc::new(Default::default()));
        let path = PathBuf::from("test.txt");

        let resource = UntypedResource::new_pending(ResourceKind::External);
        let res = manager.register(resource.clone(), path.clone(), |_, _| true);
        assert!(res.is_err());

        let resource = UntypedResource::new_ok(Uuid::new_v4(), ResourceKind::External, Stub {});
        let res = manager.register(resource.clone(), path.clone(), |_, _| true);
        assert!(res.is_ok());
    }

    #[test]
    fn resource_manager_request() {
        let manager = ResourceManager::new(Arc::new(Default::default()));
        let untyped = UntypedResource::new_ok(Uuid::new_v4(), Default::default(), Stub {});
        let res = manager.register(untyped.clone(), PathBuf::from("foo.txt"), |_, _| true);
        assert!(res.is_ok());

        let res: Resource<Stub> = manager.request(Path::new("foo.txt"));
        assert_eq!(
            res,
            Resource {
                untyped,
                phantom: PhantomData::<Stub>
            }
        );
    }

    #[test]
    fn resource_manager_request_untyped() {
        let manager = ResourceManager::new(Arc::new(Default::default()));
        let resource = UntypedResource::new_ok(Uuid::new_v4(), Default::default(), Stub {});
        let res = manager.register(resource.clone(), PathBuf::from("foo.txt"), |_, __| true);
        assert!(res.is_ok());

        let res = manager.request_untyped(Path::new("foo.txt"));
        assert_eq!(res, resource);
    }

    #[test]
    fn display_for_resource_registration_error() {
        assert_eq!(
            format!("{}", ResourceRegistrationError::AlreadyRegistered),
            "A resource is already registered!"
        );
        assert_eq!(
            format!("{}", ResourceRegistrationError::InvalidState),
            "A resource was in invalid state!"
        );
        assert_eq!(
            format!("{}", ResourceRegistrationError::UnableToRegister),
            "Unable to register the resource!"
        );
    }

    #[test]
    fn debug_for_resource_registration_error() {
        assert_eq!(
            format!("{:?}", ResourceRegistrationError::AlreadyRegistered),
            "AlreadyRegistered"
        );
        assert_eq!(
            format!("{:?}", ResourceRegistrationError::InvalidState),
            "InvalidState"
        );
        assert_eq!(
            format!("{:?}", ResourceRegistrationError::UnableToRegister),
            "UnableToRegister"
        );
    }
}
