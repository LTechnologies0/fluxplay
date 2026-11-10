//! Byte-mode QR (error level M, versions 1–10). No extra crate: the payload
//! is short (`fp1|…`) and has to render inside the app, offline.

const M: [(u8, u8, u8, u8, u8); 10] = [
    // ec, blocks1, data1, blocks2, data2
    (10, 1, 16, 0, 0),
    (16, 1, 28, 0, 0),
    (26, 1, 44, 0, 0),
    (18, 2, 32, 0, 0),
    (24, 2, 43, 0, 0),
    (16, 4, 27, 0, 0),
    (18, 4, 31, 0, 0),
    (22, 2, 38, 2, 39),
    (22, 3, 36, 2, 37),
    (26, 4, 43, 1, 44),
];

const ALIGN: [&[usize]; 10] = [
    &[],
    &[6, 18],
    &[6, 22],
    &[6, 26],
    &[6, 30],
    &[6, 34],
    &[6, 22, 38],
    &[6, 24, 42],
    &[6, 26, 46],
    &[6, 28, 50],
];

const REMAINDER: [usize; 10] = [0, 7, 7, 7, 7, 7, 0, 0, 0, 0];

/// RGBA image of `text`, quiet zone included. `None` when the text is too long.
pub fn rgba(text: &str) -> Option<(u32, Vec<u8>)> {
    let mods = matrix(text.as_bytes())?;
    let n = mods.len();
    let quiet = 4usize;
    let scale = 8usize;
    let side = (n + quiet * 2) * scale;
    let mut px = vec![255u8; side * side * 4];
    for y in 0..n {
        for x in 0..n {
            if !mods[y][x] {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let xx = (x + quiet) * scale + dx;
                    let yy = (y + quiet) * scale + dy;
                    let i = (yy * side + xx) * 4;
                    px[i] = 0;
                    px[i + 1] = 0;
                    px[i + 2] = 0;
                }
            }
        }
    }
    Some((side as u32, px))
}

fn matrix(data: &[u8]) -> Option<Vec<Vec<bool>>> {
    let version = choose_version(data.len())?;
    let vi = version - 1;
    let (ec, b1, d1, b2, d2) = M[vi];
    let capacity = b1 as usize * d1 as usize + b2 as usize * d2 as usize;
    let bytes = pack_bytes(data, version, capacity)?;

    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let mut ecs: Vec<Vec<u8>> = Vec::new();
    let mut at = 0;
    for (n, data_len) in [(b1, d1), (b2, d2)] {
        for _ in 0..n {
            let end = at + data_len as usize;
            let block = bytes[at..end].to_vec();
            at = end;
            ecs.push(reed_solomon(&block, ec as usize));
            blocks.push(block);
        }
    }
    let mut msg = Vec::new();
    let max_data = d1.max(d2) as usize;
    for i in 0..max_data {
        for block in &blocks {
            if i < block.len() {
                msg.push(block[i]);
            }
        }
    }
    for i in 0..ec as usize {
        for ecb in &ecs {
            msg.push(ecb[i]);
        }
    }

    let size = version * 4 + 17;
    let mut fun = vec![vec![false; size]; size];
    let mut dark = vec![vec![false; size]; size];
    draw_function(version, &mut fun, &mut dark);

    let mut bit_i = 0usize;
    let total_bits = msg.len() * 8 + REMAINDER[vi];
    let mut col = size as isize - 1;
    let mut upward = true;
    while col > 0 {
        if col == 6 {
            col -= 1;
        }
        for row_i in 0..size {
            let y = if upward { size - 1 - row_i } else { row_i };
            for dx in [0isize, -1] {
                let x = col + dx;
                if x < 0 || fun[y][x as usize] {
                    continue;
                }
                let on = if bit_i < msg.len() * 8 {
                    let byte = msg[bit_i / 8];
                    (byte >> (7 - (bit_i % 8))) & 1 == 1
                } else {
                    false
                };
                if bit_i < total_bits {
                    dark[y][x as usize] = on;
                    bit_i += 1;
                }
            }
        }
        upward = !upward;
        col -= 2;
    }

    let mut best: Option<(i32, Vec<Vec<bool>>)> = None;
    for mask in 0..8u8 {
        let mut m = dark.clone();
        for y in 0..size {
            for x in 0..size {
                if fun[y][x] {
                    continue;
                }
                if mask_on(mask, y, x) {
                    m[y][x] = !m[y][x];
                }
            }
        }
        draw_format(version, mask, &mut m);
        let pen = penalty(&m);
        if best.as_ref().map(|(p, _)| pen < *p).unwrap_or(true) {
            best = Some((pen, m));
        }
    }
    best.map(|(_, m)| m)
}

fn pack_bytes(data: &[u8], version: usize, capacity: usize) -> Option<Vec<u8>> {
    let mut bits: Vec<bool> = Vec::new();
    push_bits(&mut bits, 0b0100, 4);
    let len_bits = if version < 10 { 8 } else { 16 };
    if data.len() >= 1 << len_bits {
        return None;
    }
    push_bits(&mut bits, data.len() as u32, len_bits);
    for b in data {
        push_bits(&mut bits, *b as u32, 8);
    }
    let cap_bits = capacity * 8;
    if bits.len() > cap_bits {
        return None;
    }
    let term = 4.min(cap_bits - bits.len());
    push_bits(&mut bits, 0, term);
    while bits.len() % 8 != 0 {
        bits.push(false);
    }
    let mut bytes = Vec::with_capacity(capacity);
    for chunk in bits.chunks(8) {
        let mut v = 0u8;
        for (i, bit) in chunk.iter().enumerate() {
            if *bit {
                v |= 1 << (7 - i);
            }
        }
        bytes.push(v);
    }
    let mut toggle = true;
    while bytes.len() < capacity {
        bytes.push(if toggle { 0xEC } else { 0x11 });
        toggle = !toggle;
    }
    Some(bytes)
}

fn choose_version(len: usize) -> Option<usize> {
    for (i, row) in M.iter().enumerate() {
        let version = i + 1;
        let capacity = row.1 as usize * row.2 as usize + row.3 as usize * row.4 as usize;
        let header = 4 + if version < 10 { 8 } else { 16 };
        let need = (header + len * 8 + 7) / 8;
        if need <= capacity {
            return Some(version);
        }
    }
    None
}

fn push_bits(out: &mut Vec<bool>, val: u32, n: usize) {
    for i in (0..n).rev() {
        out.push(((val >> i) & 1) == 1);
    }
}

fn draw_function(version: usize, fun: &mut [Vec<bool>], dark: &mut [Vec<bool>]) {
    let size = fun.len();
    let finder = |fun: &mut [Vec<bool>], dark: &mut [Vec<bool>], ox: usize, oy: usize| {
        for dy in 0..7 {
            for dx in 0..7 {
                let x = ox + dx;
                let y = oy + dy;
                fun[y][x] = true;
                let edge = dx == 0 || dy == 0 || dx == 6 || dy == 6;
                let core = (2..5).contains(&dx) && (2..5).contains(&dy);
                dark[y][x] = edge || core;
            }
        }
    };
    finder(fun, dark, 0, 0);
    finder(fun, dark, size - 7, 0);
    finder(fun, dark, 0, size - 7);
    // Separators (light).
    for i in 0..8 {
        for (x, y) in [(i, 7), (7, i), (size - 8 + i, 7), (size - 8, i), (i, size - 8), (7, size - 8 + i)] {
            if x < size && y < size {
                fun[y][x] = true;
                dark[y][x] = false;
            }
        }
    }
    for i in 8..size - 8 {
        fun[6][i] = true;
        dark[6][i] = i % 2 == 0;
        fun[i][6] = true;
        dark[i][6] = i % 2 == 0;
    }
    let centers = ALIGN[version - 1];
    for &cy in centers {
        for &cx in centers {
            if (cx <= 8 && cy <= 8) || (cx >= size - 9 && cy <= 8) || (cx <= 8 && cy >= size - 9) {
                continue;
            }
            for dy in -2isize..=2 {
                for dx in -2isize..=2 {
                    let x = cx as isize + dx;
                    let y = cy as isize + dy;
                    fun[y as usize][x as usize] = true;
                    let edge = dx.abs() == 2 || dy.abs() == 2;
                    dark[y as usize][x as usize] = edge || (dx == 0 && dy == 0);
                }
            }
        }
    }
    // Reserve the format areas and the dark module. Format is drawn after the mask.
    for i in 0..6 {
        fun[i][8] = true;
    }
    fun[7][8] = true;
    fun[8][8] = true;
    fun[8][7] = true;
    for i in 9..15 {
        fun[8][14 - i] = true;
    }
    for i in 0..8 {
        fun[8][size - 1 - i] = true;
    }
    for i in 8..15 {
        fun[size - 15 + i][8] = true;
    }
    let dark_y = 4 * version + 9;
    fun[dark_y][8] = true;
    dark[dark_y][8] = true;
    if version >= 7 {
        for i in 0..18 {
            let a = size - 11 + i % 3;
            let b = i / 3;
            fun[b][a] = true;
            fun[a][b] = true;
        }
        let bits = version_bits(version as u32);
        for i in 0..18 {
            let on = ((bits >> i) & 1) == 1;
            let a = size - 11 + i % 3;
            let b = i / 3;
            dark[b][a] = on;
            dark[a][b] = on;
        }
    }
}

fn draw_format(version: usize, mask: u8, dark: &mut [Vec<bool>]) {
    let size = dark.len();
    let bits = format_bits(mask);
    let on = |i: usize| ((bits >> i) & 1) == 1;
    for i in 0..=5 {
        dark[i][8] = on(i);
    }
    dark[7][8] = on(6);
    dark[8][8] = on(7);
    dark[8][7] = on(8);
    for i in 9..15 {
        dark[8][14 - i] = on(i);
    }
    for i in 0..8 {
        dark[8][size - 1 - i] = on(i);
    }
    for i in 8..15 {
        dark[size - 15 + i][8] = on(i);
    }
    let _ = version;
}

fn format_bits(mask: u8) -> u16 {
    // EC level M = 00.
    let data = mask as u16;
    let mut rem = data << 10;
    for i in (0..=4).rev() {
        if (rem >> (i + 10)) & 1 == 1 {
            rem ^= 0b10100110111 << i;
        }
    }
    ((data << 10) | (rem & 0x3FF)) ^ 0b101010000010010
}

fn version_bits(version: u32) -> u32 {
    let mut rem = version << 12;
    for i in (0..=5).rev() {
        if (rem >> (i + 12)) & 1 == 1 {
            rem ^= 0b1111100100101 << i;
        }
    }
    (version << 12) | (rem & 0xFFF)
}

fn mask_on(mask: u8, y: usize, x: usize) -> bool {
    match mask {
        0 => (y + x) % 2 == 0,
        1 => y % 2 == 0,
        2 => x % 3 == 0,
        3 => (y + x) % 3 == 0,
        4 => (y / 2 + x / 3) % 2 == 0,
        5 => (y * x) % 2 + (y * x) % 3 == 0,
        6 => ((y * x) % 2 + (y * x) % 3) % 2 == 0,
        _ => ((y + x) % 2 + (y * x) % 3) % 2 == 0,
    }
}

fn penalty(m: &[Vec<bool>]) -> i32 {
    let n = m.len();
    let mut p = 0i32;
    for y in 0..n {
        let mut run = 1;
        for x in 1..n {
            if m[y][x] == m[y][x - 1] {
                run += 1;
                if run == 5 {
                    p += 3;
                } else if run > 5 {
                    p += 1;
                }
            } else {
                run = 1;
            }
        }
    }
    for x in 0..n {
        let mut run = 1;
        for y in 1..n {
            if m[y][x] == m[y - 1][x] {
                run += 1;
                if run == 5 {
                    p += 3;
                } else if run > 5 {
                    p += 1;
                }
            } else {
                run = 1;
            }
        }
    }
    for y in 0..n - 1 {
        for x in 0..n - 1 {
            let v = m[y][x];
            if m[y][x + 1] == v && m[y + 1][x] == v && m[y + 1][x + 1] == v {
                p += 3;
            }
        }
    }
    let finder = [true, false, true, true, true, false, true, false, false, false, false];
    for row in m {
        p += finder_penalty(row, &finder);
    }
    for x in 0..n {
        let col: Vec<bool> = (0..n).map(|y| m[y][x]).collect();
        p += finder_penalty(&col, &finder);
    }
    let dark_n = m.iter().flatten().filter(|c| **c).count();
    let pct = (dark_n * 100 / (n * n)) as i32;
    let k = (pct - 50).abs() / 5;
    p + k * 10
}

fn finder_penalty(line: &[bool], pat: &[bool]) -> i32 {
    let mut p = 0;
    if line.len() < pat.len() {
        return 0;
    }
    for i in 0..=line.len() - pat.len() {
        if line[i..i + pat.len()] == pat[..] {
            p += 40;
        }
    }
    p
}

fn reed_solomon(data: &[u8], ec_len: usize) -> Vec<u8> {
    let mut gen = vec![1u8];
    for i in 0..ec_len {
        gen = poly_mul(&gen, &[1, gf_pow(2, i as i32)]);
    }
    let mut res = vec![0u8; data.len() + ec_len];
    res[..data.len()].copy_from_slice(data);
    for i in 0..data.len() {
        let coef = res[i];
        if coef == 0 {
            continue;
        }
        for j in 1..gen.len() {
            res[i + j] ^= gf_mul(gen[j], coef);
        }
    }
    res[data.len()..].to_vec()
}

fn poly_mul(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; a.len() + b.len() - 1];
    for (i, &av) in a.iter().enumerate() {
        for (j, &bv) in b.iter().enumerate() {
            out[i + j] ^= gf_mul(av, bv);
        }
    }
    out
}

fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let (log, exp) = gf();
    exp[(log[a as usize] as usize + log[b as usize] as usize) % 255]
}

fn gf_pow(base: u8, expn: i32) -> u8 {
    let mut r = 1u8;
    for _ in 0..expn {
        r = gf_mul(r, base);
    }
    r
}

fn gf() -> (&'static [u8; 256], &'static [u8; 256]) {
    use std::sync::OnceLock;
    static LOG: OnceLock<[u8; 256]> = OnceLock::new();
    static EXP: OnceLock<[u8; 256]> = OnceLock::new();
    let exp = EXP.get_or_init(|| {
        let mut e = [0u8; 256];
        let mut x = 1u16;
        for i in 0..255 {
            e[i] = x as u8;
            x <<= 1;
            if x >= 256 {
                x ^= 0x11d;
            }
        }
        e[255] = e[0];
        e
    });
    let log = LOG.get_or_init(|| {
        let mut l = [0u8; 256];
        for i in 0..255 {
            l[exp[i] as usize] = i as u8;
        }
        l
    });
    (log, exp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_is_a_square_with_finders() {
        let m = matrix(b"HELLO").expect("version");
        assert!(m.len() >= 21);
        assert_eq!(m.len(), m[0].len());
        assert!(m[0][0] && m[6][0] && m[0][6]);
    }
}
