// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use gtk::{gio, glib, prelude::*};
use log::{debug, warn};

// How long the folders stay without changes before they're read again, as
// copying an album changes its files many times over
const SETTLE_TIME: Duration = Duration::from_secs(2);

// Watches the folders of the library for files that come, go or change,
// and for drives that are mounted or unmounted, and calls `changed` once
// nothing changed for `SETTLE_TIME`, for a scan to read them again. Each
// folder has a monitor of its own, as GLib's don't look into the folders
// in a folder
pub struct FolderWatch {
    monitors: RefCell<HashMap<PathBuf, gio::FileMonitor>>,
    pending: RefCell<Option<glib::SourceId>>,
    changed: Box<dyn Fn()>,
    volume_monitor: gio::VolumeMonitor,
    mount_handlers: RefCell<Vec<glib::SignalHandlerId>>,
}

impl FolderWatch {
    pub fn new(changed: impl Fn() + 'static) -> Rc<Self> {
        let watch = Rc::new(Self {
            monitors: RefCell::default(),
            pending: RefCell::default(),
            changed: Box::new(changed),
            volume_monitor: gio::VolumeMonitor::get(),
            mount_handlers: RefCell::default(),
        });

        // A music folder on a drive is there once it's mounted, and gone
        // once it's unmounted
        let weak = Rc::downgrade(&watch);
        let added = watch.volume_monitor.connect_mount_added(move |_, _| {
            if let Some(watch) = weak.upgrade() {
                watch.schedule();
            }
        });
        let weak = Rc::downgrade(&watch);
        let removed = watch.volume_monitor.connect_mount_removed(move |_, _| {
            if let Some(watch) = weak.upgrade() {
                watch.schedule();
            }
        });
        watch.mount_handlers.replace(vec![added, removed]);
        watch
    }

    // Watches these folders, and no others; returns whether it watched
    // folders already and some of these are new, as files could have come
    // into them before they were watched
    pub fn set_folders(self: &Rc<Self>, folders: &[PathBuf]) -> bool {
        let mut monitors = self.monitors.borrow_mut();
        let watching = !monitors.is_empty();
        let wanted: HashSet<&PathBuf> = folders.iter().collect();
        monitors.retain(|path, monitor| {
            let keep = wanted.contains(path);
            if !keep {
                monitor.cancel();
            }
            keep
        });

        let mut added = false;
        for folder in folders {
            if monitors.contains_key(folder) {
                continue;
            }
            match gio::File::for_path(folder).monitor_directory(
                gio::FileMonitorFlags::WATCH_MOVES,
                None::<&gio::Cancellable>,
            ) {
                Ok(monitor) => {
                    let weak = Rc::downgrade(self);
                    // Permissions and times alone don't change the tracks
                    monitor.connect_changed(move |_, _, _, event| {
                        if event != gio::FileMonitorEvent::AttributeChanged
                            && let Some(watch) = weak.upgrade()
                        {
                            watch.schedule();
                        }
                    });
                    monitors.insert(folder.clone(), monitor);
                    added = true;
                }
                Err(e) => warn!("Unable to watch {folder:?}: {e}"),
            }
        }
        debug!("Watching {} folders", monitors.len());
        watching && added
    }

    // Waits for the changes to stop
    fn schedule(self: &Rc<Self>) {
        if let Some(source) = self.pending.take() {
            source.remove();
        }
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(SETTLE_TIME, move || {
            if let Some(watch) = weak.upgrade() {
                // It ran, so it's gone
                watch.pending.take();
                (watch.changed)();
            }
        });
        self.pending.replace(Some(source));
    }
}

impl std::fmt::Debug for FolderWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FolderWatch")
            .field("folders", &self.monitors.borrow().len())
            .finish_non_exhaustive()
    }
}

impl Drop for FolderWatch {
    fn drop(&mut self) {
        for monitor in self.monitors.get_mut().values() {
            monitor.cancel();
        }
        if let Some(source) = self.pending.get_mut().take() {
            source.remove();
        }
        for handler in self.mount_handlers.get_mut().drain(..) {
            self.volume_monitor.disconnect(handler);
        }
    }
}
