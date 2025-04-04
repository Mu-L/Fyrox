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

use crate::{io::ResourceIo, loader::ResourceLoadersContainer, metadata::ResourceMetadata};
use fyrox_core::parking_lot::Mutex;
use fyrox_core::{append_extension, io::FileError, ok_or_return, warn, Uuid};
use ron::ser::PrettyConfig;
use std::sync::Arc;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub type RegistryContainer = BTreeMap<Uuid, PathBuf>;

#[allow(async_fn_in_trait)]
pub trait RegistryContainerExt: Sized {
    async fn load_from_file(path: &Path, resource_io: &dyn ResourceIo) -> Result<Self, FileError>;
    async fn save(&self, path: &Path, resource_io: &dyn ResourceIo) -> Result<(), FileError>;
}

impl RegistryContainerExt for RegistryContainer {
    async fn load_from_file(path: &Path, resource_io: &dyn ResourceIo) -> Result<Self, FileError> {
        resource_io.load_file(path).await.and_then(|metadata| {
            ron::de::from_bytes::<Self>(&metadata).map_err(|err| {
                FileError::Custom(format!(
                    "Unable to deserialize the resource registry. Reason: {:?}",
                    err
                ))
            })
        })
    }

    async fn save(&self, path: &Path, resource_io: &dyn ResourceIo) -> Result<(), FileError> {
        let string = ron::ser::to_string_pretty(self, PrettyConfig::default()).map_err(|err| {
            FileError::Custom(format!(
                "Unable to serialize resource registry! Reason: {}",
                err
            ))
        })?;
        resource_io.write_file(path, string.into_bytes()).await
    }
}

/// Resource registry is responsible for UUID mapping of resource files. It maintains a map of
/// `UUID -> Resource Path`.
#[derive(Default, Clone)]
pub struct ResourceRegistry {
    paths: RegistryContainer,
}

impl ResourceRegistry {
    pub const DEFAULT_PATH: &'static str = "./resources.registry";

    pub fn register(&mut self, uuid: Uuid, path: PathBuf) -> Option<PathBuf> {
        self.paths.insert(uuid, path)
    }

    pub fn set_container(&mut self, registry_container: RegistryContainer) {
        self.paths = registry_container;
    }

    pub fn uuid_to_path(&self, uuid: Uuid) -> Option<&Path> {
        self.paths.get(&uuid).map(|path| path.as_path())
    }

    pub fn path_to_uuid(&self, path: &Path) -> Option<Uuid> {
        self.paths
            .iter()
            .find_map(|(k, v)| if v == path { Some(*k) } else { None })
    }

    pub fn path_to_uuid_or_random(&self, path: &Path) -> Uuid {
        self.path_to_uuid(path).unwrap_or_else(|| {
            warn!(
                "There's no UUID for {} resource! Random UUID will be used, run \
                    ResourceRegistry::scan_and_update to generate resource ids!",
                path.display()
            );

            Uuid::new_v4()
        })
    }

    /// Searches for supported resources starting from the given path and builds a mapping `UUID -> Path`.
    /// If a supported resource does not have a metadata file besides it, this method will automatically
    /// add it with a new UUID and add the resource to the registry.
    ///
    /// This method does **not** load any resource, instead it checks extension of every file in the
    /// given directory, and if there's a loader for it, "remember" the resource.
    pub async fn scan(
        resource_io: Arc<dyn ResourceIo>,
        loaders: Arc<Mutex<ResourceLoadersContainer>>,
        root: impl AsRef<Path>,
    ) -> RegistryContainer {
        let registry_path = root.as_ref();
        let registry_folder = registry_path
            .parent()
            .map(|path| path.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        let mut container = RegistryContainer::default();

        let file_iterator = ok_or_return!(
            resource_io.walk_directory(&registry_folder).await,
            container
        );
        for path in file_iterator {
            if !loaders.lock().is_supported_resource(&path) {
                continue;
            }

            let metadata_path = append_extension(&path, ResourceMetadata::EXTENSION);
            let metadata =
                match ResourceMetadata::load_from_file(&metadata_path, &*resource_io).await {
                    Ok(metadata) => metadata,
                    Err(err) => {
                        warn!(
                            "Unable to load metadata for {} resource. Reason: {:?}, The metadata \
                            file will be added/recreated, do **NOT** delete it! Add it to the \
                            version control!",
                            path.display(),
                            err
                        );
                        let new_metadata = ResourceMetadata::new_with_random_id();
                        if let Err(err) = new_metadata.save(&metadata_path, &*resource_io).await {
                            warn!(
                                "Unable to save resource {} metadata. Reason: {:?}",
                                path.display(),
                                err
                            );
                        }
                        new_metadata
                    }
                };

            if container
                .insert(metadata.resource_id, path.clone())
                .is_some()
            {
                warn!(
                    "Resource UUID collision occurred for {} resource!",
                    path.display()
                );
            }
        }

        container
    }
}
