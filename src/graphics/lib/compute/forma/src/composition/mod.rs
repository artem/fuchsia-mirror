// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(feature = "gpu")]
use std::mem;
use std::{
    borrow::Cow,
    cell::{RefCell, RefMut},
    rc::Rc,
};

#[cfg(feature = "gpu")]
use renderer::{Renderer, Timings};
use rustc_hash::FxHashMap;
#[cfg(feature = "gpu")]
use surpass::rasterizer::Rasterizer;
use surpass::{
    layout::Layout,
    painter::{self, Channel, Color, LayerProps, Props, Rect},
    GeomId, Order,
};

use crate::buffer::{Buffer, BufferLayerCache};

mod backends;
mod interner;
mod layer;
mod small_bit_set;
mod state;

pub use self::{
    backends::{Backend, CpuBackend},
    layer::Layer,
};
#[cfg(feature = "gpu")]
pub use backends::GpuBackend;
#[cfg(feature = "gpu")]
use backends::StyleMap;
pub(crate) use small_bit_set::SmallBitSet;
use state::{LayerSharedState, LayerSharedStateInner};

const LINES_GARBAGE_THRESHOLD: usize = 2;

#[derive(Debug, Default)]
pub struct Composition<B: Backend = CpuBackend> {
    backend: B,
    layers: FxHashMap<Order, Layer>,
    shared_state: Rc<RefCell<LayerSharedStateInner>>,
    buffers_with_caches: Rc<RefCell<SmallBitSet>>,
}

impl Composition<CpuBackend> {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn render<L>(
        &mut self,
        buffer: &mut Buffer<'_, '_, L>,
        mut channels: [Channel; 4],
        clear_color: Color,
        crop: Option<Rect>,
    ) where
        L: Layout,
    {
        // If `clear_color` has alpha = 1 we can upgrade the alpha channel to `Channel::One`
        // in order to skip reading the alpha channel.
        if clear_color.a == 1.0 {
            channels = channels.map(|c| match c {
                Channel::Alpha => Channel::One,
                c => c,
            });
        }

        if let Some(buffer_layer_cache) = buffer.layer_cache.as_ref() {
            let tiles_len = buffer.layout.width_in_tiles() * buffer.layout.height_in_tiles();

            if buffer_layer_cache.cache.borrow().1.len() != tiles_len {
                buffer_layer_cache.cache.borrow_mut().1.resize(tiles_len, None);
                buffer_layer_cache.clear();
            }
        }

        self.compact_geom();
        self.shared_state.borrow_mut().props_interner.compact();

        let layers = &self.layers;
        let shared_state = &mut *self.shared_state.borrow_mut();
        let lines_builder = &mut shared_state.lines_builder;
        let geom_id_to_order = &shared_state.geom_id_to_order;
        let rasterizer = &mut self.backend.rasterizer;

        struct CompositionContext<'l> {
            layers: &'l FxHashMap<Order, Layer>,
            cache_id: Option<u8>,
        }

        impl LayerProps for CompositionContext<'_> {
            #[inline]
            fn get(&self, id: u32) -> Cow<'_, Props> {
                Cow::Borrowed(
                    self.layers
                        .get(&Order::new(id).expect("PixelSegment layer_id cannot overflow Order"))
                        .map(|layer| &layer.props)
                        .expect(
                            "Layers outside of HashMap should not produce visible PixelSegments",
                        ),
                )
            }

            #[inline]
            fn is_unchanged(&self, id: u32) -> bool {
                match self.cache_id {
                    None => false,
                    Some(cache_id) => self
                        .layers
                        .get(&Order::new(id).expect("PixelSegment layer_id cannot overflow Order"))
                        .map(|layer| layer.is_unchanged(cache_id))
                        .expect(
                            "Layers outside of HashMap should not produce visible PixelSegments",
                        ),
                }
            }
        }

        let context = CompositionContext {
            layers,
            cache_id: buffer.layer_cache.as_ref().map(|cache| cache.id),
        };

        // `take()` sets the RefCell's content with `Default::default()` which is cheap for Option.
        let builder = lines_builder.take().expect("lines_builder should not be None");

        *lines_builder = {
            let lines = {
                duration!("gfx", "LinesBuilder::build");
                builder.build(|id| {
                    geom_id_to_order
                        .get(&id)
                        .copied()
                        .flatten()
                        .and_then(|order| context.layers.get(&order))
                        .map(|layer| layer.inner.clone())
                })
            };

            {
                duration!("gfx", "Rasterizer::rasterize");
                rasterizer.rasterize(&lines);
            }
            {
                duration!("gfx", "Rasterizer::sort");
                rasterizer.sort();
            }

            let previous_clear_color =
                buffer.layer_cache.as_ref().and_then(|layer_cache| layer_cache.cache.borrow().0);

            let layers_per_tile = buffer.layer_cache.as_ref().map(|layer_cache| {
                RefMut::map(layer_cache.cache.borrow_mut(), |cache| &mut cache.1)
            });

            {
                duration!("gfx", "painter::for_each_row");
                painter::for_each_row(
                    buffer.layout,
                    buffer.buffer,
                    channels,
                    buffer.flusher.as_deref(),
                    previous_clear_color,
                    layers_per_tile,
                    rasterizer.segments(),
                    clear_color,
                    &crop,
                    &context,
                );
            }

            Some(lines.unwrap())
        };

        if let Some(buffer_layer_cache) = &buffer.layer_cache {
            buffer_layer_cache.cache.borrow_mut().0 = Some(clear_color);

            for layer in self.layers.values_mut() {
                layer.set_is_unchanged(buffer_layer_cache.id, layer.inner.is_enabled);
            }
        }
    }
}

#[cfg(feature = "gpu")]
impl Composition<GpuBackend> {
    pub fn new_gpu(
        device: &wgpu::Device,
        swap_chain_format: wgpu::TextureFormat,
        has_timestamp_query: bool,
    ) -> Self {
        Self {
            backend: GpuBackend {
                rasterizer: Rasterizer::default(),
                style_map: StyleMap::default(),
                renderer: Renderer::new(device, swap_chain_format, has_timestamp_query),
            },
            layers: FxHashMap::default(),
            shared_state: Rc::new(RefCell::new(LayerSharedStateInner::default())),
            buffers_with_caches: Rc::new(RefCell::new(SmallBitSet::default())),
        }
    }

    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface: &wgpu::Surface,
        width: u32,
        height: u32,
        clear_color: Color,
    ) -> Option<Timings> {
        self.compact_geom();
        self.shared_state.borrow_mut().props_interner.compact();

        let layers = &self.layers;
        let rasterizer = &mut self.backend.rasterizer;
        let shared_state = &mut *self.shared_state.borrow_mut();
        let lines_builder = &mut shared_state.lines_builder;
        let geom_id_to_order = &shared_state.geom_id_to_order;
        let builder = lines_builder.take().expect("lines_builder should not be None");

        struct CompositionContext<'l> {
            layers: &'l FxHashMap<Order, Layer>,
            cache_id: Option<u8>,
        }

        impl LayerProps for CompositionContext<'_> {
            #[inline]
            fn get(&self, id: u32) -> Cow<'_, Props> {
                Cow::Borrowed(
                    self.layers
                        .get(&Order::new(id).expect("PixelSegment layer_id cannot overflow Order"))
                        .map(|layer| &layer.props)
                        .expect(
                            "Layers outside of HashMap should not produce visible PixelSegments",
                        ),
                )
            }

            #[inline]
            fn is_unchanged(&self, id: u32) -> bool {
                match self.cache_id {
                    None => false,
                    Some(cache_id) => self
                        .layers
                        .get(&Order::new(id).expect("PixelSegment layer_id cannot overflow Order"))
                        .map(|layer| layer.is_unchanged(cache_id))
                        .expect(
                            "Layers outside of HashMap should not produce visible PixelSegments",
                        ),
                }
            }
        }

        let context = CompositionContext { layers, cache_id: None };

        *lines_builder = {
            let lines = builder.build(|id| {
                geom_id_to_order
                    .get(&id)
                    .copied()
                    .flatten()
                    .and_then(|order| context.layers.get(&order))
                    .map(|layer| layer.inner.clone())
            });

            rasterizer.rasterize(&lines);

            Some(lines.unwrap())
        };

        self.backend.style_map.populate(&self.layers);

        self.backend
            .renderer
            .render(
                device,
                queue,
                surface,
                width,
                height,
                unsafe { mem::transmute(rasterizer.segments()) },
                self.backend.style_map.style_indices(),
                self.backend.style_map.styles(),
                renderer::Color {
                    r: clear_color.r,
                    g: clear_color.g,
                    b: clear_color.b,
                    a: clear_color.a,
                },
            )
            .unwrap()
    }
}

impl<B: Backend> Composition<B> {
    #[inline]
    pub fn create_layer(&mut self) -> Layer {
        let (geom_id, props) = {
            let mut state = self.shared_state.borrow_mut();

            let geom_id = state.new_geom_id();
            let props = state.props_interner.get(Props::default());

            (geom_id, props)
        };

        Layer {
            inner: surpass::Layer::default(),
            shared_state: LayerSharedState::new(Rc::clone(&self.shared_state)),
            geom_id,
            props,
            is_unchanged: SmallBitSet::default(),
            lines_count: 0,
        }
    }

    #[inline]
    pub fn create_buffer_layer_cache(&mut self) -> Option<BufferLayerCache> {
        self.buffers_with_caches.borrow_mut().first_empty_slot().map(|id| BufferLayerCache {
            id,
            cache: Default::default(),
            buffers_with_caches: Rc::downgrade(&self.buffers_with_caches),
        })
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    #[inline]
    pub fn insert(&mut self, order: Order, mut layer: Layer) -> Option<Layer> {
        assert_eq!(
            &layer.shared_state, &self.shared_state,
            "Layer was crated by a different Composition"
        );

        layer.set_order(Some(order));

        self.layers.insert(order, layer).map(|mut layer| {
            layer.set_order(None);

            layer
        })
    }

    #[inline]
    pub fn remove(&mut self, order: Order) -> Option<Layer> {
        self.layers.remove(&order).map(|mut layer| {
            layer.set_order(None);

            layer
        })
    }

    #[inline]
    pub fn get_order_if_stored(&self, geom_id: GeomId) -> Option<Order> {
        self.shared_state.borrow().geom_id_to_order.get(&geom_id).copied().flatten()
    }

    #[inline]
    pub fn get(&self, order: Order) -> Option<&Layer> {
        self.layers.get(&order)
    }

    #[inline]
    pub fn get_mut(&mut self, order: Order) -> Option<&mut Layer> {
        self.layers.get_mut(&order)
    }

    #[inline]
    pub fn get_mut_or_insert_default(&mut self, order: Order) -> &mut Layer {
        if !self.layers.contains_key(&order) {
            let layer = self.create_layer();
            self.insert(order, layer);
        }

        self.get_mut(order).unwrap()
    }

    #[inline]
    pub fn layers(&self) -> impl ExactSizeIterator<Item = (Order, &Layer)> + '_ {
        self.layers.iter().map(|(&order, layer)| (order, layer))
    }

    #[inline]
    pub fn layers_mut(&mut self) -> impl ExactSizeIterator<Item = (Order, &mut Layer)> + '_ {
        self.layers.iter_mut().map(|(&order, layer)| (order, layer))
    }

    fn builder_len(&self) -> usize {
        self.shared_state
            .borrow()
            .lines_builder
            .as_ref()
            .expect("lines_builder should not be None")
            .len()
    }

    fn actual_len(&self) -> usize {
        self.layers.values().map(|layer| layer.lines_count).sum()
    }

    #[inline]
    pub fn compact_geom(&mut self) {
        if self.builder_len() >= self.actual_len() * LINES_GARBAGE_THRESHOLD {
            let state = &mut *self.shared_state.borrow_mut();
            let lines_builder = &mut state.lines_builder;
            let geom_id_to_order = &mut state.geom_id_to_order;

            lines_builder
                .as_mut()
                .expect("lines_builder should not be None")
                .retain(|id| geom_id_to_order.contains_key(&id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::convert::TryFrom;

    use surpass::{
        painter::{Color, RGBA},
        Path, TILE_HEIGHT, TILE_WIDTH,
    };

    use crate::{
        buffer::{layout::LinearLayout, BufferBuilder},
        Fill, FillRule, Func, GeomPresTransform, PathBuilder, Point, Style,
    };

    const BLACK_SRGB: [u8; 4] = [0x00, 0x00, 0x00, 0xFF];
    const GRAY_SRGB: [u8; 4] = [0xBB, 0xBB, 0xBB, 0xFF];
    const GRAY_ALPHA_50_SRGB: [u8; 4] = [0xBB, 0xBB, 0xBB, 0x80];
    const WHITE_ALPHA_0_SRGB: [u8; 4] = [0xFF, 0xFF, 0xFF, 0x00];
    const RED_SRGB: [u8; 4] = [0xFF, 0x00, 0x00, 0xFF];
    const GREEN_SRGB: [u8; 4] = [0x00, 0xFF, 0x00, 0xFF];
    const RED_50_GREEN_50_SRGB: [u8; 4] = [0xBB, 0xBB, 0x00, 0xFF];

    const BLACK: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
    const BLACK_ALPHA_50: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.5 };
    const GRAY: Color = Color { r: 0.5, g: 0.5, b: 0.5, a: 1.0 };
    const WHITE_TRANSPARENT: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.0 };
    const RED: Color = Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
    const GREEN: Color = Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 };

    fn pixel_path(x: i32, y: i32) -> Path {
        let mut builder = PathBuilder::new();

        builder.move_to(Point::new(x as f32, y as f32));
        builder.line_to(Point::new(x as f32, (y + 1) as f32));
        builder.line_to(Point::new((x + 1) as f32, (y + 1) as f32));
        builder.line_to(Point::new((x + 1) as f32, y as f32));
        builder.line_to(Point::new(x as f32, y as f32));

        builder.build()
    }

    fn solid(color: Color) -> Props {
        Props {
            func: Func::Draw(Style { fill: Fill::Solid(color), ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn composition_len() {
        let mut composition = Composition::new();

        assert!(composition.is_empty());
        assert_eq!(composition.len(), 0);

        composition.get_mut_or_insert_default(Order::new(0).unwrap());

        assert!(!composition.is_empty());
        assert_eq!(composition.len(), 1);
    }

    #[test]
    fn background_color_clear() {
        let mut buffer = [GREEN_SRGB].concat();
        let mut layout = LinearLayout::new(1, 4, 1);

        let mut composition = Composition::new();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            RED,
            None,
        );

        assert_eq!(buffer, [RED_SRGB].concat());
    }

    #[test]
    fn background_color_clear_when_changed() {
        let mut buffer = [GREEN_SRGB].concat();
        let mut layout = LinearLayout::new(1, 4, 1);

        let mut composition = Composition::new();
        let layer_cache = composition.create_buffer_layer_cache().unwrap();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache.clone())
                .build(),
            RGBA,
            RED,
            None,
        );

        assert_eq!(buffer, [RED_SRGB].concat());

        buffer = [GREEN_SRGB].concat();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache.clone())
                .build(),
            RGBA,
            RED,
            None,
        );

        // Skip clearing if the color is the same.
        assert_eq!(buffer, [GREEN_SRGB].concat());

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache.clone())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [BLACK_SRGB].concat());
    }

    #[test]
    fn one_pixel() {
        let mut buffer = [GREEN_SRGB; 3].concat();
        let mut layout = LinearLayout::new(3, 3 * 4, 1);

        let mut composition = Composition::new();

        let mut layer = composition.create_layer();
        layer.insert(&pixel_path(1, 0)).set_props(solid(RED));

        composition.insert(Order::new(0).unwrap(), layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            GREEN,
            None,
        );

        assert_eq!(buffer, [GREEN_SRGB, RED_SRGB, GREEN_SRGB].concat());
    }

    #[test]
    fn two_pixels_same_layer() {
        let mut buffer = [GREEN_SRGB; 3].concat();
        let mut layout = LinearLayout::new(3, 3 * 4, 1);
        let mut composition = Composition::new();

        let mut layer = composition.create_layer();
        layer.insert(&pixel_path(1, 0)).insert(&pixel_path(2, 0)).set_props(solid(RED));

        composition.insert(Order::new(0).unwrap(), layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            GREEN,
            None,
        );

        assert_eq!(buffer, [GREEN_SRGB, RED_SRGB, RED_SRGB].concat());
    }

    #[test]
    fn one_pixel_translated() {
        let mut buffer = [GREEN_SRGB; 3].concat();
        let mut layout = LinearLayout::new(3, 3 * 4, 1);

        let mut composition = Composition::new();

        let mut layer = composition.create_layer();
        layer
            .insert(&pixel_path(1, 0))
            .set_props(solid(RED))
            .set_transform(GeomPresTransform::try_from([1.0, 0.0, 0.0, 1.0, 0.5, 0.0]).unwrap());

        composition.insert(Order::new(0).unwrap(), layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            GREEN,
            None,
        );

        assert_eq!(buffer, [GREEN_SRGB, RED_50_GREEN_50_SRGB, RED_50_GREEN_50_SRGB].concat());
    }

    #[test]
    fn one_pixel_rotated() {
        let mut buffer = [GREEN_SRGB; 3].concat();
        let mut layout = LinearLayout::new(3, 3 * 4, 1);

        let mut composition = Composition::new();
        let angle = -std::f32::consts::PI / 2.0;

        let mut layer = composition.create_layer();
        layer.insert(&pixel_path(-1, 1)).set_props(solid(RED)).set_transform(
            GeomPresTransform::try_from([
                angle.cos(),
                -angle.sin(),
                angle.sin(),
                angle.cos(),
                0.0,
                0.0,
            ])
            .unwrap(),
        );

        composition.insert(Order::new(0).unwrap(), layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            GREEN,
            None,
        );

        assert_eq!(buffer, [GREEN_SRGB, RED_SRGB, GREEN_SRGB].concat());
    }

    #[test]
    fn clear_and_resize() {
        let mut buffer = [GREEN_SRGB; 4].concat();
        let mut composition = Composition::new();

        let order0 = Order::new(0).unwrap();
        let order1 = Order::new(1).unwrap();
        let order2 = Order::new(2).unwrap();

        let mut layer0 = composition.create_layer();
        layer0.insert(&pixel_path(0, 0)).set_props(solid(RED));

        composition.insert(order0, layer0);

        let mut layer1 = composition.create_layer();
        layer1.insert(&pixel_path(1, 0)).set_props(solid(RED));

        composition.insert(order1, layer1);

        let mut layer2 = composition.create_layer();
        layer2.insert(&pixel_path(2, 0)).insert(&pixel_path(3, 0)).set_props(solid(RED));

        composition.insert(order2, layer2);

        let mut layout = LinearLayout::new(4, 4 * 4, 1);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            GREEN,
            None,
        );

        assert_eq!(buffer, [RED_SRGB, RED_SRGB, RED_SRGB, RED_SRGB].concat());
        assert_eq!(composition.builder_len(), 16);
        assert_eq!(composition.actual_len(), 16);

        buffer = [GREEN_SRGB; 4].concat();

        let mut layout = LinearLayout::new(4, 4 * 4, 1);

        composition.get_mut(order0).unwrap().clear();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            GREEN,
            None,
        );

        assert_eq!(buffer, [GREEN_SRGB, RED_SRGB, RED_SRGB, RED_SRGB].concat());
        assert_eq!(composition.builder_len(), 16);
        assert_eq!(composition.actual_len(), 12);

        buffer = [GREEN_SRGB; 4].concat();

        let mut layout = LinearLayout::new(4, 4 * 4, 1);

        composition.get_mut(order2).unwrap().clear();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            GREEN,
            None,
        );

        assert_eq!(buffer, [GREEN_SRGB, RED_SRGB, GREEN_SRGB, GREEN_SRGB].concat());
        assert_eq!(composition.builder_len(), 4);
        assert_eq!(composition.actual_len(), 4);
    }

    #[test]
    fn clear_twice() {
        let mut composition = Composition::new();

        let order = Order::new(0).unwrap();

        let mut layer = composition.create_layer();
        layer.insert(&pixel_path(0, 0)).set_props(solid(RED));

        composition.insert(order, layer);

        assert_eq!(composition.actual_len(), 4);

        composition.get_mut(order).unwrap().clear();

        assert_eq!(composition.actual_len(), 0);

        composition.get_mut(order).unwrap().clear();

        assert_eq!(composition.actual_len(), 0);
    }

    #[test]
    fn insert_over_layer() {
        let mut buffer = [BLACK_SRGB; 3].concat();
        let mut layout = LinearLayout::new(3, 3 * 4, 1);

        let mut composition = Composition::new();

        let mut layer = composition.create_layer();
        layer.insert(&pixel_path(0, 0)).set_props(solid(RED));

        composition.insert(Order::new(0).unwrap(), layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [RED_SRGB, BLACK_SRGB, BLACK_SRGB].concat());

        let mut layer = composition.create_layer();
        layer.insert(&pixel_path(1, 0)).set_props(solid(GREEN));

        buffer = [BLACK_SRGB; 3].concat();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [RED_SRGB, BLACK_SRGB, BLACK_SRGB].concat());

        composition.insert(Order::new(0).unwrap(), layer);

        buffer = [BLACK_SRGB; 3].concat();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [BLACK_SRGB, GREEN_SRGB, BLACK_SRGB].concat());
    }

    #[test]
    fn layer_replace_remove() {
        let mut buffer = [BLACK_SRGB; 3].concat();
        let mut layout = LinearLayout::new(3, 3 * 4, 1);

        let mut composition = Composition::new();

        let mut layer = composition.create_layer();
        layer.insert(&pixel_path(0, 0)).set_props(solid(RED));

        composition.insert(Order::new(0).unwrap(), layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [RED_SRGB, BLACK_SRGB, BLACK_SRGB].concat());

        let mut layer = composition.create_layer();
        layer.insert(&pixel_path(1, 0)).set_props(solid(GREEN));

        let _old_layer = composition.insert(Order::new(0).unwrap(), layer);

        buffer = [BLACK_SRGB; 3].concat();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [BLACK_SRGB, GREEN_SRGB, BLACK_SRGB].concat());

        let _old_layer = composition.remove(Order::new(0).unwrap());

        buffer = [BLACK_SRGB; 3].concat();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [BLACK_SRGB, BLACK_SRGB, BLACK_SRGB].concat());
    }

    #[test]
    fn layer_clear() {
        let mut buffer = [BLACK_SRGB; 3].concat();
        let mut layout = LinearLayout::new(3, 3 * 4, 1);

        let mut composition = Composition::new();

        let order = Order::new(0).unwrap();

        let mut layer = composition.create_layer();
        layer.insert(&pixel_path(0, 0)).set_props(solid(RED));

        composition.insert(order, layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [RED_SRGB, BLACK_SRGB, BLACK_SRGB].concat());

        composition.get_mut(order).unwrap().insert(&pixel_path(1, 0));

        buffer = [BLACK_SRGB; 3].concat();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [RED_SRGB, RED_SRGB, BLACK_SRGB].concat());

        composition.get_mut(order).unwrap().clear();

        buffer = [BLACK_SRGB; 3].concat();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [BLACK_SRGB, BLACK_SRGB, BLACK_SRGB].concat());

        composition.get_mut(order).unwrap().insert(&pixel_path(2, 0));

        buffer = [BLACK_SRGB; 3].concat();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [BLACK_SRGB, BLACK_SRGB, RED_SRGB].concat());
    }

    #[test]
    fn geom_id() {
        let mut composition = Composition::new();

        let mut layer = composition.create_layer();

        layer.insert(&PathBuilder::new().build());
        let geom_id0 = layer.geom_id();

        layer.insert(&PathBuilder::new().build());
        let geom_id1 = layer.geom_id();

        assert_eq!(geom_id0, geom_id1);

        layer.clear();

        assert_ne!(layer.geom_id(), geom_id0);

        layer.insert(&PathBuilder::new().build());
        let geom_id2 = layer.geom_id();

        assert_ne!(geom_id0, geom_id2);

        let order = Order::new(0).unwrap();
        composition.insert(order, layer);

        assert_eq!(composition.get_order_if_stored(geom_id2), Some(order));

        let layer = composition.create_layer();
        composition.insert(order, layer);

        assert_eq!(composition.get_order_if_stored(geom_id2), None);
    }

    #[test]
    fn srgb_alpha_blending() {
        let mut buffer = [BLACK_SRGB; 3].concat();
        let mut layout = LinearLayout::new(3, 3 * 4, 1);

        let mut composition = Composition::new();

        let mut layer = composition.create_layer();
        layer.insert(&pixel_path(0, 0)).set_props(solid(BLACK_ALPHA_50));

        composition.insert(Order::new(0).unwrap(), layer);

        let mut layer = composition.create_layer();

        layer.insert(&pixel_path(1, 0)).set_props(solid(GRAY));

        composition.insert(Order::new(1).unwrap(), layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            WHITE_TRANSPARENT,
            None,
        );

        assert_eq!(buffer, [GRAY_ALPHA_50_SRGB, GRAY_SRGB, WHITE_ALPHA_0_SRGB].concat());
    }

    #[test]
    fn render_changed_layers_only() {
        let mut buffer = [BLACK_SRGB; 3 * TILE_WIDTH * TILE_HEIGHT].concat();
        let mut layout = LinearLayout::new(3 * TILE_WIDTH, 3 * TILE_WIDTH * 4, TILE_HEIGHT);
        let mut composition = Composition::new();
        let layer_cache = composition.create_buffer_layer_cache();

        let mut layer = composition.create_layer();
        layer
            .insert(&pixel_path(0, 0))
            .insert(&pixel_path(TILE_WIDTH as i32, 0))
            .set_props(solid(RED));

        composition.insert(Order::new(0).unwrap(), layer);

        let order = Order::new(1).unwrap();

        let mut layer = composition.create_layer();
        layer
            .insert(&pixel_path(TILE_WIDTH as i32 + 1, 0))
            .insert(&pixel_path(2 * TILE_WIDTH as i32, 0))
            .set_props(solid(GREEN));

        composition.insert(order, layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache.clone().unwrap())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer[0..4], RED_SRGB);
        assert_eq!(buffer[TILE_WIDTH * 4..TILE_WIDTH * 4 + 4], RED_SRGB);
        assert_eq!(buffer[(TILE_WIDTH + 1) * 4..(TILE_WIDTH + 1) * 4 + 4], GREEN_SRGB);
        assert_eq!(buffer[2 * TILE_WIDTH * 4..2 * TILE_WIDTH * 4 + 4], GREEN_SRGB);

        let mut buffer = [BLACK_SRGB; 3 * TILE_WIDTH * TILE_HEIGHT].concat();

        composition.get_mut(order).unwrap().set_props(solid(RED));

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache.unwrap())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer[0..4], BLACK_SRGB);
        assert_eq!(buffer[TILE_WIDTH * 4..TILE_WIDTH * 4 + 4], RED_SRGB);
        assert_eq!(buffer[(TILE_WIDTH + 1) * 4..(TILE_WIDTH + 1) * 4 + 4], RED_SRGB);
        assert_eq!(buffer[2 * TILE_WIDTH * 4..2 * TILE_WIDTH * 4 + 4], RED_SRGB);
    }

    #[test]
    fn insert_remove_same_order_will_not_render_again() {
        let mut buffer = [BLACK_SRGB; 3].concat();
        let mut layout = LinearLayout::new(3, 3 * 4, 1);

        let mut composition = Composition::new();
        let layer_cache = composition.create_buffer_layer_cache().unwrap();

        let mut layer = composition.create_layer();
        layer.insert(&pixel_path(0, 0)).set_props(solid(RED));

        composition.insert(Order::new(0).unwrap(), layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache.clone())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [RED_SRGB, BLACK_SRGB, BLACK_SRGB].concat());

        let layer = composition.remove(Order::new(0).unwrap()).unwrap();
        composition.insert(Order::new(0).unwrap(), layer);

        buffer = [BLACK_SRGB; 3].concat();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache.clone())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer, [BLACK_SRGB, BLACK_SRGB, BLACK_SRGB].concat());
    }

    #[test]
    fn clear_emptied_tiles() {
        let mut buffer = [BLACK_SRGB; 2 * TILE_WIDTH * TILE_HEIGHT].concat();
        let mut layout = LinearLayout::new(2 * TILE_WIDTH, 2 * TILE_WIDTH * 4, TILE_HEIGHT);
        let mut composition = Composition::new();
        let layer_cache = composition.create_buffer_layer_cache();

        let order = Order::new(0).unwrap();

        let mut layer = composition.create_layer();
        layer
            .insert(&pixel_path(0, 0))
            .set_props(solid(RED))
            .insert(&pixel_path(TILE_WIDTH as i32, 0));

        composition.insert(order, layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache.clone().unwrap())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer[0..4], RED_SRGB);

        composition.get_mut(order).unwrap().set_transform(
            GeomPresTransform::try_from([1.0, 0.0, 0.0, 1.0, TILE_WIDTH as f32, 0.0]).unwrap(),
        );

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache.clone().unwrap())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer[0..4], BLACK_SRGB);

        composition.get_mut(order).unwrap().set_transform(
            GeomPresTransform::try_from([1.0, 0.0, 0.0, 1.0, -(TILE_WIDTH as f32), 0.0]).unwrap(),
        );

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache.clone().unwrap())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer[0..4], RED_SRGB);

        composition.get_mut(order).unwrap().set_transform(
            GeomPresTransform::try_from([1.0, 0.0, 0.0, 1.0, 0.0, TILE_HEIGHT as f32]).unwrap(),
        );

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache.unwrap())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer[0..4], BLACK_SRGB);
    }

    #[test]
    fn separate_layer_caches() {
        let mut buffer = [BLACK_SRGB; TILE_WIDTH * TILE_HEIGHT].concat();
        let mut layout = LinearLayout::new(TILE_WIDTH, TILE_WIDTH * 4, TILE_HEIGHT);
        let mut composition = Composition::new();
        let layer_cache0 = composition.create_buffer_layer_cache();
        let layer_cache1 = composition.create_buffer_layer_cache();

        let order = Order::new(0).unwrap();

        let mut layer = composition.create_layer();
        layer.insert(&pixel_path(0, 0)).set_props(solid(RED));

        composition.insert(order, layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache0.clone().unwrap())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer[0..4], RED_SRGB);

        let mut buffer = [BLACK_SRGB; TILE_WIDTH * TILE_HEIGHT].concat();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache0.clone().unwrap())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer[0..4], BLACK_SRGB);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache1.clone().unwrap())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer[0..4], RED_SRGB);

        composition
            .get_mut(order)
            .unwrap()
            .set_transform(GeomPresTransform::try_from([1.0, 0.0, 0.0, 1.0, 1.0, 0.0]).unwrap());

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache0.unwrap())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer[0..4], BLACK_SRGB);
        assert_eq!(buffer[4..8], RED_SRGB);

        let mut buffer = [BLACK_SRGB; TILE_WIDTH * TILE_HEIGHT].concat();

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout)
                .layer_cache(layer_cache1.unwrap())
                .build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer[0..4], BLACK_SRGB);
        assert_eq!(buffer[4..8], RED_SRGB);
    }

    #[test]
    fn even_odd() {
        let mut builder = PathBuilder::new();

        builder.move_to(Point::new(0.0, 0.0));
        builder.line_to(Point::new(0.0, TILE_HEIGHT as f32));
        builder.line_to(Point::new(3.0 * TILE_WIDTH as f32, TILE_HEIGHT as f32));
        builder.line_to(Point::new(3.0 * TILE_WIDTH as f32, 0.0));
        builder.line_to(Point::new(TILE_WIDTH as f32, 0.0));
        builder.line_to(Point::new(TILE_WIDTH as f32, TILE_HEIGHT as f32));
        builder.line_to(Point::new(2.0 * TILE_WIDTH as f32, TILE_HEIGHT as f32));
        builder.line_to(Point::new(2.0 * TILE_WIDTH as f32, 0.0));
        builder.line_to(Point::new(0.0, 0.0));

        let path = builder.build();

        let mut buffer = [BLACK_SRGB; 3 * TILE_WIDTH * TILE_HEIGHT].concat();
        let mut layout = LinearLayout::new(3 * TILE_WIDTH, 3 * TILE_WIDTH * 4, TILE_HEIGHT);

        let mut composition = Composition::new();

        let mut layer = composition.create_layer();
        layer.insert(&path).set_props(Props {
            fill_rule: FillRule::EvenOdd,
            func: Func::Draw(Style { fill: Fill::Solid(RED), ..Default::default() }),
        });

        composition.insert(Order::new(0).unwrap(), layer);

        composition.render(
            &mut BufferBuilder::new(&mut buffer, &mut layout).build(),
            RGBA,
            BLACK,
            None,
        );

        assert_eq!(buffer[0..4], RED_SRGB);
        assert_eq!(buffer[TILE_WIDTH * 4..(TILE_WIDTH * 4 + 4)], BLACK_SRGB);
        assert_eq!(buffer[2 * TILE_WIDTH * 4..2 * TILE_WIDTH * 4 + 4], RED_SRGB);
    }
}
