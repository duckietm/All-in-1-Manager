//! Packet builders that write exactly what the emulator's composers write
//! (CatalogPagesListComposer, CatalogPageComposer + the layout classes, CatalogItem.serialize,
//! CatalogFeaturedPage.serialize), plus the classic layout Habbo's own server uses.
#![allow(dead_code)]

use catalog_rip_core::wire::{Packet, Writer};

pub struct Node {
    pub visible: bool,
    pub icon: i32,
    pub id: i32,
    pub parent: i32,
    pub name: &'static str,
    pub caption: &'static str,
    pub offers: Vec<i32>,
    pub children: Vec<Node>,
}

pub fn node(
    id: i32,
    parent: i32,
    name: &'static str,
    caption: &'static str,
    icon: i32,
    children: Vec<Node>,
) -> Node {
    Node {
        visible: true,
        icon,
        id,
        parent,
        name,
        caption,
        offers: vec![],
        children,
    }
}

fn write_node(w: Writer, n: &Node, with_parent: bool) -> Writer {
    let mut w = w.bool(n.visible).i32(n.icon).i32(n.id);
    if with_parent {
        w = w.i32(n.parent);
    }
    w = w
        .string(n.name)
        .string(n.caption)
        .i32(n.offers.len() as i32);
    for o in &n.offers {
        w = w.i32(*o);
    }
    w = w.i32(n.children.len() as i32);
    for c in &n.children {
        w = write_node(w, c, with_parent);
    }
    w
}

/// CatalogPagesListComposer (`with_parent` = the emulator fork; false = classic Arcturus/Habbo).
pub fn index_packet(
    header: u16,
    mode: &str,
    roots: &[Node],
    with_parent: bool,
    new_additions: bool,
) -> Packet {
    let mut w = Writer::new(header).bool(true).i32(0).i32(-1);
    if with_parent {
        w = w.i32(-1);
    }
    w = w.string("root").string("").i32(0).i32(roots.len() as i32);
    for r in roots {
        w = write_node(w, r, with_parent);
    }
    w.bool(new_additions).string(mode).packet()
}

pub enum Prod {
    Furni {
        kind: &'static str,
        class_id: i32,
        extra: &'static str,
        amount: i32,
        limited: Option<(i32, i32)>,
    },
    Badge(&'static str),
}

pub struct Item {
    pub id: i32,
    pub name: &'static str,
    pub rent: bool,
    pub credits: i32,
    pub points: i32,
    pub points_type: i32,
    pub giftable: bool,
    pub products: Vec<Prod>,
    pub club: i32,
    pub have_offer: bool,
    pub item_ids: &'static str,
}

pub fn item(id: i32, name: &'static str, credits: i32, products: Vec<Prod>) -> Item {
    Item {
        id,
        name,
        rent: false,
        credits,
        points: 0,
        points_type: 0,
        giftable: true,
        products,
        club: 0,
        have_offer: true,
        item_ids: "",
    }
}

/// CatalogItem.serialize; `extended` adds the fork's item ids string and have-offer flag.
fn write_item(mut w: Writer, i: &Item, extended: bool) -> Writer {
    w = w
        .i32(i.id)
        .string(i.name)
        .bool(i.rent)
        .i32(i.credits)
        .i32(i.points)
        .i32(i.points_type)
        .bool(i.giftable)
        .i32(i.products.len() as i32);
    for p in &i.products {
        match p {
            Prod::Badge(code) => w = w.string("b").string(code),
            Prod::Furni {
                kind,
                class_id,
                extra,
                amount,
                limited,
            } => {
                w = w
                    .string(kind)
                    .i32(*class_id)
                    .string(extra)
                    .i32(*amount)
                    .bool(limited.is_some());
                if let Some((stack, sells)) = limited {
                    w = w.i32(*stack).i32(stack - sells);
                }
            }
        }
    }
    w = w
        .i32(i.club)
        .bool(i.have_offer)
        .bool(false)
        .string(&format!("{}.png", i.name));
    if extended {
        w = w.string(i.item_ids).bool(i.have_offer);
    }
    w
}

pub struct Featured {
    pub slot: i32,
    pub caption: &'static str,
    pub image: &'static str,
    pub kind: i32,
    pub page_name: &'static str,
    pub page_id: i32,
    pub product: &'static str,
    pub expire: i32,
}

#[allow(clippy::too_many_arguments)]
/// CatalogPageComposer: id, mode, layout.serialize (layout, images, texts), items, offer id,
/// season flag, frontpage items for the frontpage layouts.
pub fn page_packet(
    header: u16,
    id: i32,
    mode: &str,
    layout: &str,
    images: &[&str],
    texts: &[&str],
    items: &[Item],
    featured: Option<&[Featured]>,
    extended: bool,
) -> Packet {
    let mut w = Writer::new(header)
        .i32(id)
        .string(mode)
        .string(layout)
        .i32(images.len() as i32);
    for s in images {
        w = w.string(s);
    }
    w = w.i32(texts.len() as i32);
    for s in texts {
        w = w.string(s);
    }
    w = w.i32(items.len() as i32);
    for i in items {
        w = write_item(w, i, extended);
    }
    w = w.i32(-1).bool(false);
    if let Some(featured) = featured {
        w = w.i32(featured.len() as i32);
        for f in featured {
            w = w.i32(f.slot).string(f.caption).string(f.image).i32(f.kind);
            w = match f.kind {
                0 => w.string(f.page_name),
                1 => w.i32(f.page_id),
                _ => w.string(f.product),
            };
            w = w.i32(f.expire);
        }
    }
    w.packet()
}

// ------------------------------------------------------------------ the test catalogue

pub const GET_INDEX: u16 = 1195;
pub const GET_PAGE: u16 = 412;
pub const INDEX: u16 = 1032;
pub const PAGE: u16 = 804;

/// NORMAL index (emulator layout, with parent ids): frontpage 1; furni 2 > 3, 4, disabled (-1) > 5; 6; 7.
pub fn normal_roots() -> Vec<Node> {
    let mut disabled = node(
        -1,
        2,
        "hidden_cat",
        "Hidden category",
        5,
        vec![node(5, -1, "hidden_child", "Hidden child", 6, vec![])],
    );
    disabled.visible = false;
    let mut furni = node(
        2,
        -1,
        "furni",
        "Furni",
        2,
        vec![
            node(3, 2, "shop_3x3", "Shop 3x3", 3, vec![]),
            node(4, 2, "rentables", "Rentables", 4, vec![]),
            disabled,
        ],
    );
    furni.offers = vec![300, 301];
    vec![
        node(1, -1, "front_page", "Front Page", 1, vec![]),
        furni,
        node(6, -1, "slow_page", "Slow page", 7, vec![]),
        node(7, -1, "dead_page", "Dead page", 8, vec![]),
    ]
}

/// BUILDERS_CLUB index (classic layout): addons 100, and rentables 4 again (a page in both modes).
pub fn builders_roots() -> Vec<Node> {
    vec![
        node(100, -1, "bc_addons", "BC Addons", 9, vec![]),
        node(4, -1, "rentables", "Rentables", 4, vec![]),
    ]
}

pub fn shop_items() -> Vec<Item> {
    let mut limited = item(
        300,
        "rare_dragon_ltd",
        50,
        vec![Prod::Furni {
            kind: "s",
            class_id: 4001,
            extra: "",
            amount: 1,
            limited: Some((100, 37)),
        }],
    );
    limited.giftable = false;
    let mut badge = item(301, "badge_ACH_Test", 0, vec![Prod::Badge("ACH_Test1")]);
    badge.points = 25;
    badge.points_type = 5;
    let mut bundle = item(
        302,
        "bundle_o'brien",
        25,
        vec![
            Prod::Furni {
                kind: "s",
                class_id: 4002,
                extra: "",
                amount: 2,
                limited: None,
            },
            Prod::Furni {
                kind: "i",
                class_id: 5001,
                extra: "poster 7",
                amount: 1,
                limited: None,
            },
        ],
    );
    bundle.club = 1;
    bundle.have_offer = false;
    bundle.item_ids = "12:2;13:1";
    let wall = item(
        303,
        "wallpaper_single_101",
        3,
        vec![Prod::Furni {
            kind: "i",
            class_id: 3001,
            extra: "101",
            amount: 1,
            limited: None,
        }],
    );
    let mut many = item(
        304,
        "chair_x5",
        5,
        vec![Prod::Furni {
            kind: "s",
            class_id: 4003,
            extra: "",
            amount: 5,
            limited: None,
        }],
    );
    many.item_ids = "14";
    vec![limited, badge, bundle, wall, many]
}

pub fn frontpage_featured() -> Vec<Featured> {
    vec![
        Featured {
            slot: 1,
            caption: "New furni!",
            image: "catalogue/feature_cata_vert_new.png",
            kind: 0,
            page_name: "shop_3x3",
            page_id: 0,
            product: "",
            expire: 0,
        },
        Featured {
            slot: 2,
            caption: "Dragon",
            image: "catalogue/feature_cata_hort_dragon.png",
            kind: 1,
            page_name: "",
            page_id: 300,
            product: "",
            expire: 3600,
        },
        Featured {
            slot: 3,
            caption: "Credits",
            image: "catalogue/feature_cata_hort_credits.png",
            kind: 2,
            page_name: "",
            page_id: 0,
            product: "credits_pack_1",
            expire: 0,
        },
    ]
}

/// The answer to GetCatalogPage, or None for "no answer".
pub fn page_answer(id: i32, mode: &str) -> Option<Packet> {
    let extended = mode == "NORMAL";
    Some(match id {
        1 => page_packet(
            PAGE,
            1,
            mode,
            "frontpage4",
            &["catalog_frontpage_headline_shop_EN", "front_teaser"],
            &["Redeem a voucher code here:", "Second text", "Teaser text"],
            &[],
            Some(&frontpage_featured()),
            extended,
        ),
        2 => page_packet(
            PAGE,
            2,
            mode,
            "default_3x3",
            &["catalog_header_furni", "", ""],
            &["Furni text", "", ""],
            &[],
            None,
            extended,
        ),
        3 => page_packet(
            PAGE,
            3,
            mode,
            "default_3x3",
            &[
                "catalog_header_roombuilder",
                "cr_infopic3",
                "catalog_special_txtbg1",
            ],
            &["It's a \"shop\"\nline 2", "Details \\ backslash", "Teaser"],
            &shop_items(),
            None,
            extended,
        ),
        4 => page_packet(
            PAGE,
            4,
            mode,
            "info_rentables",
            &["catalog_rentables_header"],
            &["Rent one", "Rent two"],
            &[],
            None,
            extended,
        ),
        5 => page_packet(
            PAGE,
            5,
            mode,
            "recolorable_furniture",
            &["h5", "t5", "s5"],
            &["a", "b", "c"],
            &[item(
                500,
                "color_chair",
                3,
                vec![Prod::Furni {
                    kind: "s",
                    class_id: 4004,
                    extra: "",
                    amount: 1,
                    limited: None,
                }],
            )],
            None,
            extended,
        ),
        6 => page_packet(
            PAGE,
            6,
            mode,
            "pets",
            &["pets_h", "pets_t"],
            &["p1", "p2", "pd", "pt"],
            &[],
            None,
            extended,
        ),
        100 => page_packet(
            PAGE,
            100,
            mode,
            "builders_club_addons",
            &["bc_header", "bc_teaser", ""],
            &["BC text", "", ""],
            &[item(
                1000,
                "bc_table",
                0,
                vec![Prod::Furni {
                    kind: "s",
                    class_id: 4002,
                    extra: "",
                    amount: 1,
                    limited: None,
                }],
            )],
            None,
            extended,
        ),
        _ => return None,
    })
}

// ------------------------------------------------------------------ retro variants

/// An index packet without the trailing mode string (`keep_flag`), or without the
/// new-additions flag as well (`!keep_flag`), as older retro servers send it.
pub fn without_index_tail(packet: Packet, mode: &str, keep_flag: bool) -> Packet {
    let mut body = packet.body;
    body.truncate(body.len() - 2 - mode.len());
    if !keep_flag {
        body.pop();
    }
    Packet {
        header: packet.header,
        body,
    }
}

/// A page packet without the mode string after the page id (pre-2015 R63 servers).
pub fn without_page_mode(packet: Packet) -> Packet {
    let len = u16::from_be_bytes([packet.body[4], packet.body[5]]) as usize;
    let mut body = packet.body[..4].to_vec();
    body.extend_from_slice(&packet.body[6 + len..]);
    Packet {
        header: packet.header,
        body,
    }
}

// ------------------------------------------------------------------ G-Earth over std::net

/// The G-Earth client's [`Transport`] over a blocking `std::net::TcpStream` (tests only; the
/// plugin uses the host's TCP connection).
pub struct StdTransport(pub std::net::TcpStream);

impl StdTransport {
    pub fn connect(port: u16) -> StdTransport {
        let stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_nodelay(true).ok();
        StdTransport(stream)
    }

    /// Closes the sending side and reads until the peer closes too, so everything sent was
    /// read before the socket goes (closing with unread data can reset the connection).
    pub fn finish(self) {
        use std::io::Read;
        let mut s = self.0;
        s.shutdown(std::net::Shutdown::Write).ok();
        s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .ok();
        let mut buf = [0u8; 4096];
        while matches!(s.read(&mut buf), Ok(n) if n > 0) {}
    }
}

impl catalog_rip_core::transport::Transport for StdTransport {
    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        use std::io::Write;
        self.0.write_all(data).map_err(|e| e.to_string())
    }

    fn recv(&mut self, max: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
        use std::io::{ErrorKind, Read};
        let mut buf = vec![0u8; max];
        // A zero read timeout is refused by std: zero means "only what is there already".
        if timeout_ms == 0 {
            self.0.set_nonblocking(true).map_err(|e| e.to_string())?;
        } else {
            self.0
                .set_read_timeout(Some(std::time::Duration::from_millis(timeout_ms.into())))
                .map_err(|e| e.to_string())?;
        }
        let read = self.0.read(&mut buf);
        if timeout_ms == 0 {
            self.0.set_nonblocking(false).map_err(|e| e.to_string())?;
        }
        match read {
            Ok(n) => {
                buf.truncate(n);
                Ok(Some(buf))
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// G-Earth's side of the socket: reads whole frames, keeping partial ones between calls.
pub struct FrameReader {
    pub stream: std::net::TcpStream,
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new(stream: std::net::TcpStream) -> FrameReader {
        FrameReader {
            stream,
            buf: Vec::new(),
        }
    }

    fn take_frame(&mut self) -> Option<Packet> {
        if self.buf.len() < 4 {
            return None;
        }
        let len = i32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if self.buf.len() < 4 + len {
            return None;
        }
        let frame: Vec<u8> = self.buf.drain(..4 + len).collect();
        Some(Packet {
            header: u16::from_be_bytes([frame[4], frame[5]]),
            body: frame[6..].to_vec(),
        })
    }

    /// The next frame. `Ok(None)`: nothing within `timeout` (None waits for ever);
    /// `Err(())`: the connection closed or failed.
    pub fn next(&mut self, timeout: Option<std::time::Duration>) -> Result<Option<Packet>, ()> {
        use std::io::{ErrorKind, Read};
        let deadline = timeout.map(|t| std::time::Instant::now() + t);
        loop {
            if let Some(frame) = self.take_frame() {
                return Ok(Some(frame));
            }
            let wait = match deadline {
                Some(d) => {
                    let left = d.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        return Ok(None);
                    }
                    Some(left)
                }
                None => None,
            };
            self.stream.set_read_timeout(wait).map_err(|_| ())?;
            let mut chunk = [0u8; 16 * 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(()),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Ok(None)
                }
                Err(_) => return Err(()),
            }
        }
    }

    /// Blocks until the next frame; None when the connection closed.
    pub fn read_frame(&mut self) -> Option<Packet> {
        self.next(None).ok().flatten()
    }

    pub fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        self.stream.write_all(data)
    }
}
