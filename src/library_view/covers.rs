// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// The covers in the grid grow with the width of the columns, between
// these sizes, at each step of the zoom from the smallest (`COVER_ZOOM`,
// with 0 for the normal size, 160 to 240). GridView picks how many columns it has by the covers'
// smallest size, and stretches them to fill the row, so each step starts
// where the one before ends: when they overlapped, a step could leave the
// same columns, as wide as before
pub(super) const COVER_SIZES: [(i32, i32); 4] = [(90, 120), (120, 160), (160, 240), (240, 320)];
pub(super) const COVER_ZOOM: zoom::ZoomSteps = zoom::ZoomSteps {
    key: "cover-zoom",
    min: -2,
    max: 1,
};

// The widest the grid gets, at any step: six columns of the largest covers
// at the normal size. Past it, it stays centered, rather than spreading
// the covers apart. The grid keeps 30 rows of cells bound, so each column
// makes scrolling slower, even when fewer of them fit
const MAX_GRID_WIDTH: i32 = 6 * (240 + COVER_SPACING);

// The space around each cover in the grid, from the margin of the cells
// and the padding of their content in style.css, and the padding of the
// grid on each side
const COVER_SPACING: i32 = 2 * (8 + 8);
const GRID_PADDING: i32 = 12;

// The number of covers kept in memory at the largest size the grid shows
// them: the ones on screen, the ones read ahead a screenful above and below
// them, and the last ones scrolled past; keeping all of them would take
// hundreds of megabytes. Smaller covers take less, so more of them are
// kept, as more fit on screen (`cache_capacity`)
const MAX_CACHED_COVERS: usize = 150;

// The cells waiting for each cover to load, with the ID of the album each
// showed then, keyed by the path of the cover
pub(super) type PendingCovers = HashMap<String, Vec<(String, glib::WeakRef<AlbumCell>)>>;

// The covers loaded most recently
#[derive(Debug)]
pub(crate) struct TextureCache {
    textures: HashMap<String, gdk::Texture>,
    order: VecDeque<String>,
    capacity: usize,
}

impl Default for TextureCache {
    fn default() -> Self {
        Self {
            textures: HashMap::new(),
            order: VecDeque::new(),
            capacity: MAX_CACHED_COVERS,
        }
    }
}

impl TextureCache {
    fn get(&mut self, path: &str) -> Option<gdk::Texture> {
        let texture = self.textures.get(path)?.clone();
        // Move it to the back of the queue, as it's in use again
        if let Some(pos) = self.order.iter().position(|p| p == path) {
            let path = self.order.remove(pos).unwrap();
            self.order.push_back(path);
        }
        Some(texture)
    }

    fn insert(&mut self, path: String, texture: gdk::Texture) {
        if self.textures.insert(path.clone(), texture).is_none() {
            self.order.push_back(path);
        }
        self.trim();
    }

    fn trim(&mut self) {
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.textures.remove(&oldest);
            }
        }
    }

    // The covers it keeps, which take about the same memory at any size
    fn capacity(&self) -> usize {
        self.capacity
    }

    fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
        self.trim();
    }

    // Whether the cover is in the cache, without counting it as used
    fn contains(&self, path: &str) -> bool {
        self.textures.contains_key(path)
    }

    pub(super) fn clear(&mut self) {
        self.textures.clear();
        self.order.clear();
    }
}

impl LibraryView {
    // Shows the cover of the album in the cell, which has just come on
    // screen, once it has been read in a separate thread, and reads ahead
    // the covers around it. Nothing waits for the main loop to be idle: it
    // never is while the grid scrolls, as GTK binds its cells again at about
    // every frame
    pub(super) fn show_cover(&self, cell: &AlbumCell) {
        self.show_cell_cover(cell);
        if let Some(position) = cell.position() {
            self.read_ahead(position, position);
        }
    }

    // Shows the cover of the album in the cell, from the cache, or once
    // it's read
    fn show_cell_cover(&self, cell: &AlbumCell) {
        let Some(album) = cell.album() else {
            return;
        };
        if let Some(path) = album.cover_path_str() {
            let texture = self.imp().textures.borrow_mut().get(path);
            match texture {
                Some(texture) => cell.picture().set_paintable(Some(&texture)),
                None => self.read_cover(path.to_owned(), Some((album.id(), cell))),
            }
        }
    }

    // Reads ahead the covers of the albums a screenful before and after
    // those from `first` to `last`, which have just come on screen, into
    // the cache:
    // GridView only shows a cell once it's within a few pixels of the
    // screen, which would leave the albums coming into view without their
    // covers while they're read. Those on screen and a screenful on each
    // side have to fit in the cache, or the covers read ahead would push
    // out the ones on screen: while the grid opens, GTK maps a few hundred
    // cells before it hides those off screen, and a large screen can show
    // as many
    fn read_ahead(&self, first: u32, last: u32) {
        let imp = self.imp();
        let Some(model) = imp.grid_view.model() else {
            return;
        };
        let capacity = imp.textures.borrow().capacity();
        let screenful = imp.mapped_cells.get().clamp(1, (capacity / 3) as u32);
        let end = last
            .saturating_add(screenful)
            .min(model.n_items().saturating_sub(1));
        for position in first.saturating_sub(screenful)..=end {
            if let Some(album) = model.item(position).and_downcast::<Album>()
                && let Some(path) = album.cover_path_str()
                && !imp.textures.borrow().contains(path)
                && !imp.pending_covers.borrow().contains_key(path)
            {
                self.read_cover(path.to_owned(), None);
            }
        }
    }

    // Reads the cover in a separate thread, into the cache, and for the
    // cell showing the album with this ID, if there's one; the cell can
    // show another album by the time it's read
    fn read_cover(&self, path: String, cell: Option<(String, &AlbumCell)>) {
        let imp = self.imp();
        if let Some(texture) = imp.textures.borrow_mut().get(&path) {
            if let Some((_, cell)) = cell {
                cell.picture().set_paintable(Some(&texture));
            }
            return;
        }

        // Another cell, or reading ahead, is already waiting for the same
        // cover
        let waiting = cell.map(|(id, cell)| (id, cell.downgrade()));
        let mut pending = imp.pending_covers.borrow_mut();
        if let Some(others) = pending.get_mut(&path) {
            others.extend(waiting);
            return;
        }
        pending.insert(path.clone(), waiting.into_iter().collect());

        let size = self.cover_pixels();
        let current = imp.cover_generation.clone();
        let generation = current.load(Ordering::Relaxed);
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let texture = {
                    let path = path.clone();
                    let current = current.clone();
                    gio::spawn_blocking(move || {
                        // Not if it's no longer wanted by the time it would
                        // be read, as quick steps of the zoom leave many
                        (current.load(Ordering::Relaxed) == generation)
                            .then(|| cover_image::read_thumbnail(Path::new(&path), size))
                            .flatten()
                    })
                    .await
                    .ok()
                    .flatten()
                };

                let imp = this.imp();
                // Read at a size the covers are no longer shown at; the
                // cells waiting for it read it again
                if current.load(Ordering::Relaxed) != generation {
                    return;
                }
                let waiting = imp.pending_covers.borrow_mut().remove(&path);
                let Some(texture) = texture else {
                    return;
                };

                for (id, cell) in waiting.into_iter().flatten() {
                    if let Some(cell) = cell.upgrade()
                        && cell.album().is_some_and(|album| album.id_str() == id)
                        && cell.picture().is_mapped()
                    {
                        cell.picture().set_paintable(Some(&texture));
                    }
                }
                imp.textures.borrow_mut().insert(path, texture);
            }
        ));
    }

    // Drops the covers in the cache, and those still being read, which are
    // no longer the ones to show
    pub(super) fn forget_covers(&self) {
        let imp = self.imp();
        imp.cover_generation.fetch_add(1, Ordering::Relaxed);
        imp.textures.borrow_mut().clear();
        imp.pending_covers.borrow_mut().clear();
    }

    // The width the covers are read at, in the pixels of the screen the
    // grid is on: as large as the grid shows them
    fn cover_pixels(&self) -> u32 {
        let (_, size) = grid_cover_sizes(self.imp().cover_zoom.get());
        (size * self.scale_factor()).max(1) as u32
    }

    // The covers in the cache are at a size they're no longer shown at:
    // they're read again for the cells on screen, which keep the ones they
    // show until then, and the covers still being read are dropped
    pub(super) fn reset_covers(&self) {
        self.forget_covers();
        let cells = mapped_album_cells(self.upcast_ref());
        for cell in &cells {
            self.show_cell_cover(cell);
        }
        // Around them all at once, rather than around each
        let positions: Vec<u32> = cells.iter().filter_map(AlbumCell::position).collect();
        if let (Some(&first), Some(&last)) = (positions.iter().min(), positions.iter().max()) {
            self.read_ahead(first, last);
        }
    }

    // The covers are as large as the `cover-zoom` setting says, with Ctrl
    // and the mouse wheel over them as well as with the keys
    pub(super) fn setup_zoom(&self) {
        self.app_settings().connect_changed(
            Some(COVER_ZOOM.key),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| this.apply_cover_zoom()
            ),
        );
        self.apply_cover_zoom();

        zoom::add_zoom_input(
            &*self.imp().albums_stack,
            clone!(
                #[weak(rename_to = this)]
                self,
                move |zoom| {
                    this.zoom_covers(zoom);
                }
            ),
        );
    }

    // Zooms the covers of the grid and of the works a step, or back to
    // their normal size; false when they're at that end already
    pub(crate) fn zoom_covers(&self, zoom: Zoom) -> bool {
        COVER_ZOOM.zoom(self.app_settings(), zoom)
    }

    fn apply_cover_zoom(&self) {
        let imp = self.imp();
        let step = COVER_ZOOM.step(self.app_settings());
        imp.cover_zoom.set(step);
        imp.zoom_row.set_percent(cover_zoom_percent(step));

        // Past the width of the most columns with the largest covers, the
        // grid stays centered, rather than spreading the covers apart
        let (_, max_size) = grid_cover_sizes(step);
        let columns = max_columns(max_size);
        imp.grid_view.set_max_columns(columns);
        let grid_width = columns as i32 * (max_size + COVER_SPACING) + 2 * GRID_PADDING;
        imp.grid_clamp.set_maximum_size(grid_width);
        imp.grid_clamp.set_tightening_threshold(grid_width);

        imp.textures
            .borrow_mut()
            .set_capacity(cache_capacity(max_size));
        self.notify_cover_zoom();
        self.reset_covers();

        // Larger than the covers were made, they're made again, and until
        // then shown scaled up; back from a zoom in, they're made smaller.
        // The first scan makes them at the size they're zoomed to
        let made = imp.thumbnail_size.get();
        if made != 0 && thumbnail_sizes(step).update(Some(made)) != ThumbnailUpdate::Keep {
            self.start_scan(false);
        }

        let enable = |name: &str, on: bool| {
            if let Some(action) = imp
                .actions
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(on);
            }
        };
        // The size between them stays on, so that it reads as the size
        enable("zoom-in", step < COVER_ZOOM.max);
        enable("zoom-out", step > COVER_ZOOM.min);

        // The columns change, and the album with the focus could go out
        // of view
        if let Some(position) =
            utils::focused_child::<AlbumCell>(&*imp.grid_view).and_then(|cell| cell.position())
        {
            utils::when_laid_out(&*imp.grid_view, move |grid| {
                grid.scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
            });
        }
    }
}

// The sizes the covers of the grid grow between at a step of the zoom
pub(super) fn grid_cover_sizes(step: i32) -> (i32, i32) {
    COVER_ZOOM.at(&COVER_SIZES, step)
}

// The size of the covers at a step of the zoom, in percent of the normal
// size, by their largest size
fn cover_zoom_percent(step: i32) -> u32 {
    let ((_, max), (_, normal)) = (grid_cover_sizes(step), grid_cover_sizes(0));
    (f64::from(max) * 100.0 / f64::from(normal)).round() as u32
}

// The most columns the grid has with covers of this largest size, so that
// it's never wider than MAX_GRID_WIDTH
pub(super) fn max_columns(max_size: i32) -> u32 {
    (f64::from(MAX_GRID_WIDTH) / f64::from(max_size + COVER_SPACING)).round() as u32
}

// How many covers the cache keeps with covers of this largest size: about
// as many pixels as at the normal size
pub(super) fn cache_capacity(max_size: i32) -> usize {
    let (_, normal) = grid_cover_sizes(0);
    let ratio = f64::from(normal) / f64::from(max_size);
    (MAX_CACHED_COVERS as f64 * ratio * ratio).round() as usize
}

// The cells under the widget that show an album's cover, the grid's and
// those of an open work, which are mapped; GridView keeps the cells off
// screen bound, but unmapped
fn mapped_album_cells(widget: &gtk::Widget) -> Vec<AlbumCell> {
    let mut cells = Vec::new();
    let mut child = widget.first_child();
    while let Some(widget) = child {
        match widget.downcast_ref::<AlbumCell>() {
            Some(cell) if cell.picture().is_mapped() => cells.push(cell.clone()),
            Some(_) => {}
            None if widget.is_mapped() => cells.extend(mapped_album_cells(&widget)),
            None => {}
        }
        child = widget.next_sibling();
    }
    cells
}

// The widths the covers of the grid are made at: the largest the grid shows
// them, in the pixels of the monitor with the largest scale, and at a scale
// of 2, which the scan keeps (`ThumbnailSizes::update`). A cover twice as
// large as it's shown takes four times the memory, and the time to read.
// Zoomed out, they're made at the normal size, as they're scaled down as
// they're read, and zooming back in would make them all again
pub(super) fn thumbnail_sizes(zoom: i32) -> library::ThumbnailSizes {
    let scale = gdk::Display::default()
        .and_then(|display| {
            display
                .monitors()
                .iter::<gdk::Monitor>()
                .flatten()
                .map(|monitor| monitor.scale_factor())
                .max()
        })
        .unwrap_or(1)
        .clamp(1, 2);
    let (_, max_size) = grid_cover_sizes(zoom.max(0));
    library::ThumbnailSizes {
        wanted: (max_size * scale) as u32,
        scale_2: (max_size * 2) as u32,
    }
}
