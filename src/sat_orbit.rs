//! Satellite orbits from public element sets, worked out on this PC: the SGP4 propagator for
//! near-earth orbits and SDP4's deep-space terms (lunar and solar perturbations, the 12-hour and
//! 24-hour resonances) for periods of 225 minutes and more, as revised by Vallado, Crawford,
//! Hujsak and Kelso ("Revisiting Spacetrack Report #3", AIAA 2006-6753; the same algorithm as
//! the reference code's "improved" mode). Checked against the report's own verification vectors
//! (`tests`), a near-earth satellite, a Molniya, a geostationary one and two in between.
//!
//! Around it: CelesTrak's OMM JSON and classic two-line sets, the TEME frame to the turning
//! Earth (GMST), WGS-84 latitude/longitude/altitude, the look angles from Sir's position, a
//! low-precision Sun, the Earth's shadow and the visible passes of a satellite ("when can I see
//! the ISS?").

use chrono::NaiveDateTime;
use serde_json::Value;
use std::f64::consts::PI;

const TWOPI: f64 = 2.0 * PI;
const DEG: f64 = PI / 180.0;
const X2O3: f64 = 2.0 / 3.0;
/// WGS-72, which the element sets are fitted with.
const MU: f64 = 398_600.8;
const RE: f64 = 6378.135;
const J2: f64 = 0.001_082_616;
const J3: f64 = -0.000_002_538_81;
const J4: f64 = -0.000_001_655_97;
/// The Earth's turn in radians per minute (the deep-space resonance's sidereal rate).
const RPTIM: f64 = 4.375_269_088_011_299_66e-3;
/// WGS-84, for latitudes and altitudes on the map.
const WGS84_A: f64 = 6378.137;
const WGS84_F: f64 = 1.0 / 298.257_223_563;
/// Days from 1949-12-31 00:00 UT (SGP4's epoch count) to 1970-01-01 00:00 UT.
const DAYS_1950_TO_UNIX: f64 = 7306.0;
/// Julian date of 1970-01-01 00:00 UT.
const JD_UNIX: f64 = 2_440_587.5;
const AU_KM: f64 = 149_597_870.7;

// ------------------------------------------------------------------------- element sets ----

/// One satellite's mean elements (as CelesTrak publishes them).
#[derive(Debug, Clone, PartialEq)]
pub struct Elements {
    pub name: String,
    pub norad: u32,
    /// The international designator, "1998-067A".
    pub object_id: String,
    /// The epoch, seconds since 1970 (UTC).
    pub epoch_unix: f64,
    /// Revolutions a day.
    pub mean_motion: f64,
    pub eccentricity: f64,
    /// Degrees.
    pub inclination: f64,
    pub raan: f64,
    pub arg_perigee: f64,
    pub mean_anomaly: f64,
    /// The drag term (1 / Earth radii).
    pub bstar: f64,
}

impl Elements {
    /// Minutes per revolution.
    pub fn period_min(&self) -> f64 {
        if self.mean_motion > 0.0 {
            1440.0 / self.mean_motion
        } else {
            f64::INFINITY
        }
    }
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// "2026-11-08T04:12:35.430048" (UTC) as seconds since 1970.
pub fn parse_epoch(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('Z');
    let t = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f").or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")).ok()?;
    let utc = t.and_utc();
    Some(utc.timestamp() as f64 + utc.timestamp_subsec_nanos() as f64 * 1e-9)
}

/// One OMM record (CelesTrak's `FORMAT=json`).
pub fn parse_omm_record(v: &Value) -> Option<Elements> {
    let el = Elements {
        name: v["OBJECT_NAME"].as_str().unwrap_or("").trim().to_string(),
        norad: number(&v["NORAD_CAT_ID"])? as u32,
        object_id: v["OBJECT_ID"].as_str().unwrap_or("").trim().to_string(),
        epoch_unix: parse_epoch(v["EPOCH"].as_str()?)?,
        mean_motion: number(&v["MEAN_MOTION"])?,
        eccentricity: number(&v["ECCENTRICITY"])?,
        inclination: number(&v["INCLINATION"])?,
        raan: number(&v["RA_OF_ASC_NODE"])?,
        arg_perigee: number(&v["ARG_OF_PERICENTER"])?,
        mean_anomaly: number(&v["MEAN_ANOMALY"])?,
        bstar: number(&v["BSTAR"]).unwrap_or(0.0),
    };
    (el.mean_motion > 0.0 && (0.0..1.0).contains(&el.eccentricity)).then_some(el)
}

/// A CelesTrak OMM JSON answer (an array of records); unreadable records are left out.
pub fn parse_omm(v: &Value) -> Vec<Elements> {
    v.as_array().map(|list| list.iter().filter_map(parse_omm_record).collect()).unwrap_or_default()
}

/// "11873-3" (an assumed decimal point and a power of ten) as 0.11873e-3.
#[cfg_attr(not(test), allow(dead_code))]
fn assumed_decimal(field: &str) -> Option<f64> {
    let f = field.trim();
    if f.is_empty() {
        return Some(0.0);
    }
    let (sign, rest) = match f.as_bytes()[0] {
        b'-' => (-1.0, &f[1..]),
        b'+' => (1.0, &f[1..]),
        _ => (1.0, f),
    };
    let split = rest.rfind(['-', '+']).filter(|i| *i > 0).unwrap_or(rest.len());
    let mantissa: f64 = format!("0.{}", rest[..split].trim()).parse().ok()?;
    let exponent: i32 = if split < rest.len() { rest[split..].parse().ok()? } else { 0 };
    Some(sign * mantissa * 10f64.powi(exponent))
}

/// A classic two-line element set (the verification vectors come as these).
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_tle(name: &str, line1: &str, line2: &str) -> Option<Elements> {
    fn col(s: &str, a: usize, b: usize) -> &str {
        s.get(a..b.min(s.len())).map(str::trim).unwrap_or("")
    }
    if !line1.starts_with('1') || !line2.starts_with('2') || line1.len() < 61 || line2.len() < 63 {
        return None;
    }
    let norad: u32 = col(line1, 2, 7).parse().ok()?;
    let year: i32 = col(line1, 18, 20).parse().ok()?;
    let year = if year < 57 { 2000 + year } else { 1900 + year };
    let day: f64 = col(line1, 20, 32).parse().ok()?;
    let jan1 = chrono::NaiveDate::from_ymd_opt(year, 1, 1)?.and_hms_opt(0, 0, 0)?.and_utc().timestamp() as f64;
    Some(Elements {
        name: name.trim().to_string(),
        norad,
        object_id: col(line1, 9, 17).to_string(),
        epoch_unix: jan1 + (day - 1.0) * 86_400.0,
        mean_motion: col(line2, 52, 63).parse().ok()?,
        eccentricity: format!("0.{}", col(line2, 26, 33)).parse().ok()?,
        inclination: col(line2, 8, 16).parse().ok()?,
        raan: col(line2, 17, 25).parse().ok()?,
        arg_perigee: col(line2, 34, 42).parse().ok()?,
        mean_anomaly: col(line2, 43, 51).parse().ok()?,
        bstar: assumed_decimal(col(line1, 53, 61))?,
    })
}

// ------------------------------------------------------------------------- the propagator ----

/// The deep-space terms (SDP4): lunar-solar periodics, secular rates and the resonance.
#[derive(Debug, Clone, Default)]
struct Deep {
    e3: f64,
    ee2: f64,
    se2: f64,
    se3: f64,
    sgh2: f64,
    sgh3: f64,
    sgh4: f64,
    sh2: f64,
    sh3: f64,
    si2: f64,
    si3: f64,
    sl2: f64,
    sl3: f64,
    sl4: f64,
    xgh2: f64,
    xgh3: f64,
    xgh4: f64,
    xh2: f64,
    xh3: f64,
    xi2: f64,
    xi3: f64,
    xl2: f64,
    xl3: f64,
    xl4: f64,
    zmol: f64,
    zmos: f64,
    /// 0: none, 1: one day (geosynchronous), 2: half a day (Molniya).
    irez: u8,
    d2201: f64,
    d2211: f64,
    d3210: f64,
    d3222: f64,
    d4410: f64,
    d4422: f64,
    d5220: f64,
    d5232: f64,
    d5421: f64,
    d5433: f64,
    dedt: f64,
    didt: f64,
    dmdt: f64,
    dnodt: f64,
    domdt: f64,
    del1: f64,
    del2: f64,
    del3: f64,
    xfact: f64,
    xlamo: f64,
}

/// What `dscom` works out for `dsinit` (the Sun's and the Moon's geometry at the epoch).
#[derive(Debug, Clone, Default)]
struct DsCom {
    sinim: f64,
    cosim: f64,
    emsq: f64,
    s1: f64,
    s2: f64,
    s3: f64,
    s4: f64,
    s5: f64,
    ss1: f64,
    ss2: f64,
    ss3: f64,
    ss4: f64,
    ss5: f64,
    sz1: f64,
    sz3: f64,
    sz11: f64,
    sz13: f64,
    sz21: f64,
    sz23: f64,
    sz31: f64,
    sz33: f64,
    z1: f64,
    z3: f64,
    z11: f64,
    z13: f64,
    z21: f64,
    z23: f64,
    z31: f64,
    z33: f64,
}

/// A satellite ready to propagate: SGP4 (and SDP4's terms for a deep-space orbit).
#[derive(Debug, Clone, Default)]
pub struct Sgp4 {
    pub epoch_unix: f64,
    xke: f64,
    j3oj2: f64,
    bstar: f64,
    ecco: f64,
    argpo: f64,
    inclo: f64,
    mo: f64,
    no_unkozai: f64,
    nodeo: f64,
    /// Perigee under 220 km: the simplified drag terms.
    isimp: bool,
    /// A period of 225 minutes or more.
    deep: bool,
    aycof: f64,
    con41: f64,
    cc1: f64,
    cc4: f64,
    cc5: f64,
    d2: f64,
    d3: f64,
    d4: f64,
    delmo: f64,
    eta: f64,
    argpdot: f64,
    omgcof: f64,
    sinmao: f64,
    t2cof: f64,
    t3cof: f64,
    t4cof: f64,
    t5cof: f64,
    x1mth2: f64,
    x7thm1: f64,
    mdot: f64,
    nodedot: f64,
    xlcof: f64,
    xmcof: f64,
    nodecf: f64,
    gsto: f64,
    d: Deep,
}

/// A position and velocity in the TEME frame (km, km/s).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StateVector {
    pub r: [f64; 3],
    pub v: [f64; 3],
}

impl Sgp4 {
    /// Initialises the propagator (`sgp4init`), checked with a propagation to the epoch.
    pub fn new(el: &Elements) -> Result<Self, String> {
        let xke = 60.0 / (RE * RE * RE / MU).sqrt();
        let j3oj2 = J3 / J2;
        let no_kozai = el.mean_motion / (1440.0 / TWOPI);
        if no_kozai <= 0.0 || !(0.0..1.0).contains(&el.eccentricity) {
            return Err(format!("{}: unusable elements", el.name));
        }
        let mut s = Sgp4 {
            epoch_unix: el.epoch_unix,
            xke,
            j3oj2,
            bstar: el.bstar,
            ecco: el.eccentricity,
            argpo: el.arg_perigee * DEG,
            inclo: el.inclination * DEG,
            mo: el.mean_anomaly * DEG,
            nodeo: el.raan * DEG,
            ..Default::default()
        };
        let epoch = el.epoch_unix / 86_400.0 + DAYS_1950_TO_UNIX;
        let ss = 78.0 / RE + 1.0;
        let qzms2t = ((120.0 - 78.0) / RE).powi(4);

        // initl: the un-Kozai'd mean motion and the epoch's sidereal time.
        let eccsq = s.ecco * s.ecco;
        let omeosq = 1.0 - eccsq;
        let rteosq = omeosq.sqrt();
        let cosio = s.inclo.cos();
        let cosio2 = cosio * cosio;
        let ak = (xke / no_kozai).powf(X2O3);
        let d1 = 0.75 * J2 * (3.0 * cosio2 - 1.0) / (rteosq * omeosq);
        let del = d1 / (ak * ak);
        let adel = ak * (1.0 - del * del - del * (1.0 / 3.0 + 134.0 * del * del / 81.0));
        let del = d1 / (adel * adel);
        let no = no_kozai / (1.0 + del);
        s.no_unkozai = no;
        let ao = (xke / no).powf(X2O3);
        let sinio = s.inclo.sin();
        let po = ao * omeosq;
        let con42 = 1.0 - 5.0 * cosio2;
        s.con41 = -con42 - cosio2 - cosio2;
        let posq = po * po;
        let rp = ao * (1.0 - s.ecco);
        s.gsto = gstime(epoch + 2_433_281.5);

        s.isimp = rp < 220.0 / RE + 1.0;
        let mut sfour = ss;
        let mut qzms24 = qzms2t;
        let perige = (rp - 1.0) * RE;
        if perige < 156.0 {
            sfour = if perige < 98.0 { 20.0 } else { perige - 78.0 };
            qzms24 = ((120.0 - sfour) / RE).powi(4);
            sfour = sfour / RE + 1.0;
        }
        let pinvsq = 1.0 / posq;
        let tsi = 1.0 / (ao - sfour);
        s.eta = ao * s.ecco * tsi;
        let etasq = s.eta * s.eta;
        let eeta = s.ecco * s.eta;
        let psisq = (1.0 - etasq).abs();
        let coef = qzms24 * tsi.powi(4);
        let coef1 = coef / psisq.powf(3.5);
        let cc2 = coef1 * no * (ao * (1.0 + 1.5 * etasq + eeta * (4.0 + etasq)) + 0.375 * J2 * tsi / psisq * s.con41 * (8.0 + 3.0 * etasq * (8.0 + etasq)));
        s.cc1 = s.bstar * cc2;
        let cc3 = if s.ecco > 1.0e-4 { -2.0 * coef * tsi * j3oj2 * no * sinio / s.ecco } else { 0.0 };
        s.x1mth2 = 1.0 - cosio2;
        s.cc4 = 2.0
            * no
            * coef1
            * ao
            * omeosq
            * (s.eta * (2.0 + 0.5 * etasq) + s.ecco * (0.5 + 2.0 * etasq)
                - J2 * tsi / (ao * psisq) * (-3.0 * s.con41 * (1.0 - 2.0 * eeta + etasq * (1.5 - 0.5 * eeta)) + 0.75 * s.x1mth2 * (2.0 * etasq - eeta * (1.0 + etasq)) * (2.0 * s.argpo).cos()));
        s.cc5 = 2.0 * coef1 * ao * omeosq * (1.0 + 2.75 * (etasq + eeta) + eeta * etasq);
        let cosio4 = cosio2 * cosio2;
        let temp1 = 1.5 * J2 * pinvsq * no;
        let temp2 = 0.5 * temp1 * J2 * pinvsq;
        let temp3 = -0.46875 * J4 * pinvsq * pinvsq * no;
        s.mdot = no + 0.5 * temp1 * rteosq * s.con41 + 0.0625 * temp2 * rteosq * (13.0 - 78.0 * cosio2 + 137.0 * cosio4);
        s.argpdot = -0.5 * temp1 * con42 + 0.0625 * temp2 * (7.0 - 114.0 * cosio2 + 395.0 * cosio4) + temp3 * (3.0 - 36.0 * cosio2 + 49.0 * cosio4);
        let xhdot1 = -temp1 * cosio;
        s.nodedot = xhdot1 + (0.5 * temp2 * (4.0 - 19.0 * cosio2) + 2.0 * temp3 * (3.0 - 7.0 * cosio2)) * cosio;
        let xpidot = s.argpdot + s.nodedot;
        s.omgcof = s.bstar * cc3 * s.argpo.cos();
        s.xmcof = if s.ecco > 1.0e-4 { -X2O3 * coef * s.bstar / eeta } else { 0.0 };
        s.nodecf = 3.5 * omeosq * xhdot1 * s.cc1;
        s.t2cof = 1.5 * s.cc1;
        s.xlcof = xlcof(j3oj2, sinio, cosio);
        s.aycof = -0.5 * j3oj2 * sinio;
        s.delmo = (1.0 + s.eta * s.mo.cos()).powi(3);
        s.sinmao = s.mo.sin();
        s.x7thm1 = 7.0 * cosio2 - 1.0;

        if TWOPI / no >= 225.0 {
            s.deep = true;
            s.isimp = true;
            let ds = s.dscom(epoch);
            // (The reference calls dpper here with init 'y': it changes nothing at the epoch.)
            s.dsinit(&ds, xpidot, eccsq);
        }
        if !s.isimp {
            let cc1sq = s.cc1 * s.cc1;
            s.d2 = 4.0 * ao * tsi * cc1sq;
            let temp = s.d2 * tsi * s.cc1 / 3.0;
            s.d3 = (17.0 * ao + sfour) * temp;
            s.d4 = 0.5 * temp * ao * tsi * (221.0 * ao + 31.0 * sfour) * s.cc1;
            s.t3cof = s.d2 + 2.0 * cc1sq;
            s.t4cof = 0.25 * (3.0 * s.d3 + s.cc1 * (12.0 * s.d2 + 10.0 * cc1sq));
            s.t5cof = 0.2 * (3.0 * s.d4 + 12.0 * s.cc1 * s.d3 + 6.0 * s.d2 * s.d2 + 15.0 * cc1sq * (2.0 * s.d2 + cc1sq));
        }
        s.propagate(0.0).map_err(|e| format!("{}: {e}", el.name))?;
        Ok(s)
    }

    /// Whether SDP4's deep-space terms apply (a period of 225 minutes or more).
    pub fn is_deep_space(&self) -> bool {
        self.deep
    }

    /// The Sun's and the Moon's terms at the epoch (`dscom`).
    fn dscom(&mut self, epoch: f64) -> DsCom {
        const ZES: f64 = 0.01675;
        const ZEL: f64 = 0.05490;
        const C1SS: f64 = 2.986_479_7e-6;
        const C1L: f64 = 4.796_806_5e-7;
        const ZSINIS: f64 = 0.397_854_16;
        const ZCOSIS: f64 = 0.917_448_67;
        const ZCOSGS: f64 = 0.194_590_5;
        const ZSINGS: f64 = -0.980_884_58;
        let (nm, em) = (self.no_unkozai, self.ecco);
        let (snodm, cnodm) = self.nodeo.sin_cos();
        let (sinomm, cosomm) = self.argpo.sin_cos();
        let (sinim, cosim) = self.inclo.sin_cos();
        let emsq = em * em;
        let betasq = 1.0 - emsq;
        let rtemsq = betasq.sqrt();
        let day = epoch + 18_261.5;
        let xnodce = (4.523_602_0 - 9.242_202_9e-4 * day).rem_euclid(TWOPI);
        let (stem, ctem) = xnodce.sin_cos();
        let zcosil = 0.913_751_64 - 0.035_680_96 * ctem;
        let zsinil = (1.0 - zcosil * zcosil).sqrt();
        let zsinhl = 0.089_683_511 * stem / zsinil;
        let zcoshl = (1.0 - zsinhl * zsinhl).sqrt();
        let gam = 5.835_151_4 + 0.001_944_368_0 * day;
        let zx = 0.397_854_16 * stem / zsinil;
        let zy = zcoshl * ctem + 0.917_448_67 * zsinhl * stem;
        let zx = gam + zx.atan2(zy) - xnodce;
        let (zsingl, zcosgl) = zx.sin_cos();

        let (mut zcosg, mut zsing, mut zcosi, mut zsini, mut zcosh, mut zsinh) = (ZCOSGS, ZSINGS, ZCOSIS, ZSINIS, cnodm, snodm);
        let mut cc = C1SS;
        let xnoi = 1.0 / nm;
        let mut out = DsCom { sinim, cosim, emsq, ..Default::default() };
        // The second round's (lunar) values are what dscom hands on as s1…s7 and z1…z33.
        let (mut s1, mut s2, mut s3, mut s4, mut s5, mut s6, mut s7) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let (mut z1, mut z2, mut z3, mut z11, mut z12, mut z13, mut z21, mut z22, mut z23, mut z31, mut z32, mut z33) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let (mut ss1, mut ss2, mut ss3, mut ss4, mut ss6, mut ss7) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let (mut sz1, mut sz2, mut sz3, mut sz11, mut sz12, mut sz13, mut sz21, mut sz22, mut sz23, mut sz31, mut sz32, mut sz33) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for lsflg in 1..=2 {
            let a1 = zcosg * zcosh + zsing * zcosi * zsinh;
            let a3 = -zsing * zcosh + zcosg * zcosi * zsinh;
            let a7 = -zcosg * zsinh + zsing * zcosi * zcosh;
            let a8 = zsing * zsini;
            let a9 = zsing * zsinh + zcosg * zcosi * zcosh;
            let a10 = zcosg * zsini;
            let a2 = cosim * a7 + sinim * a8;
            let a4 = cosim * a9 + sinim * a10;
            let a5 = -sinim * a7 + cosim * a8;
            let a6 = -sinim * a9 + cosim * a10;
            let x1 = a1 * cosomm + a2 * sinomm;
            let x2 = a3 * cosomm + a4 * sinomm;
            let x3 = -a1 * sinomm + a2 * cosomm;
            let x4 = -a3 * sinomm + a4 * cosomm;
            let x5 = a5 * sinomm;
            let x6 = a6 * sinomm;
            let x7 = a5 * cosomm;
            let x8 = a6 * cosomm;
            z31 = 12.0 * x1 * x1 - 3.0 * x3 * x3;
            z32 = 24.0 * x1 * x2 - 6.0 * x3 * x4;
            z33 = 12.0 * x2 * x2 - 3.0 * x4 * x4;
            z1 = 3.0 * (a1 * a1 + a2 * a2) + z31 * emsq;
            z2 = 6.0 * (a1 * a3 + a2 * a4) + z32 * emsq;
            z3 = 3.0 * (a3 * a3 + a4 * a4) + z33 * emsq;
            z11 = -6.0 * a1 * a5 + emsq * (-24.0 * x1 * x7 - 6.0 * x3 * x5);
            z12 = -6.0 * (a1 * a6 + a3 * a5) + emsq * (-24.0 * (x2 * x7 + x1 * x8) - 6.0 * (x3 * x6 + x4 * x5));
            z13 = -6.0 * a3 * a6 + emsq * (-24.0 * x2 * x8 - 6.0 * x4 * x6);
            z21 = 6.0 * a2 * a5 + emsq * (24.0 * x1 * x5 - 6.0 * x3 * x7);
            z22 = 6.0 * (a4 * a5 + a2 * a6) + emsq * (24.0 * (x2 * x5 + x1 * x6) - 6.0 * (x4 * x7 + x3 * x8));
            z23 = 6.0 * a4 * a6 + emsq * (24.0 * x2 * x6 - 6.0 * x4 * x8);
            z1 = z1 + z1 + betasq * z31;
            z2 = z2 + z2 + betasq * z32;
            z3 = z3 + z3 + betasq * z33;
            s3 = cc * xnoi;
            s2 = -0.5 * s3 / rtemsq;
            s4 = s3 * rtemsq;
            s1 = -15.0 * em * s4;
            s5 = x1 * x3 + x2 * x4;
            s6 = x2 * x3 + x1 * x4;
            s7 = x2 * x4 - x1 * x3;
            if lsflg == 1 {
                (ss1, ss2, ss3, ss4, out.ss5, ss6, ss7) = (s1, s2, s3, s4, s5, s6, s7);
                (sz1, sz2, sz3, sz11, sz12, sz13, sz21, sz22, sz23, sz31, sz32, sz33) = (z1, z2, z3, z11, z12, z13, z21, z22, z23, z31, z32, z33);
                zcosg = zcosgl;
                zsing = zsingl;
                zcosi = zcosil;
                zsini = zsinil;
                zcosh = zcoshl * cnodm + zsinhl * snodm;
                zsinh = snodm * zcoshl - cnodm * zsinhl;
                cc = C1L;
            }
        }
        let d = &mut self.d;
        d.zmol = (4.719_967_2 + 0.229_971_50 * day - gam).rem_euclid(TWOPI);
        d.zmos = (6.256_583_7 + 0.017_201_977 * day).rem_euclid(TWOPI);
        d.se2 = 2.0 * ss1 * ss6;
        d.se3 = 2.0 * ss1 * ss7;
        d.si2 = 2.0 * ss2 * sz12;
        d.si3 = 2.0 * ss2 * (sz13 - sz11);
        d.sl2 = -2.0 * ss3 * sz2;
        d.sl3 = -2.0 * ss3 * (sz3 - sz1);
        d.sl4 = -2.0 * ss3 * (-21.0 - 9.0 * emsq) * ZES;
        d.sgh2 = 2.0 * ss4 * sz32;
        d.sgh3 = 2.0 * ss4 * (sz33 - sz31);
        d.sgh4 = -18.0 * ss4 * ZES;
        d.sh2 = -2.0 * ss2 * sz22;
        d.sh3 = -2.0 * ss2 * (sz23 - sz21);
        d.ee2 = 2.0 * s1 * s6;
        d.e3 = 2.0 * s1 * s7;
        d.xi2 = 2.0 * s2 * z12;
        d.xi3 = 2.0 * s2 * (z13 - z11);
        d.xl2 = -2.0 * s3 * z2;
        d.xl3 = -2.0 * s3 * (z3 - z1);
        d.xl4 = -2.0 * s3 * (-21.0 - 9.0 * emsq) * ZEL;
        d.xgh2 = 2.0 * s4 * z32;
        d.xgh3 = 2.0 * s4 * (z33 - z31);
        d.xgh4 = -18.0 * s4 * ZEL;
        d.xh2 = -2.0 * s2 * z22;
        d.xh3 = -2.0 * s2 * (z23 - z21);
        out.s1 = s1;
        out.s2 = s2;
        out.s3 = s3;
        out.s4 = s4;
        out.s5 = s5;
        out.ss1 = ss1;
        out.ss2 = ss2;
        out.ss3 = ss3;
        out.ss4 = ss4;
        out.sz1 = sz1;
        out.sz3 = sz3;
        out.sz11 = sz11;
        out.sz13 = sz13;
        out.sz21 = sz21;
        out.sz23 = sz23;
        out.sz31 = sz31;
        out.sz33 = sz33;
        out.z1 = z1;
        out.z3 = z3;
        out.z11 = z11;
        out.z13 = z13;
        out.z21 = z21;
        out.z23 = z23;
        out.z31 = z31;
        out.z33 = z33;
        out
    }

    /// The deep-space secular rates and the resonance's coefficients (`dsinit`, at the epoch).
    fn dsinit(&mut self, c: &DsCom, xpidot: f64, eccsq: f64) {
        const Q22: f64 = 1.789_167_9e-6;
        const Q31: f64 = 2.146_074_8e-6;
        const Q33: f64 = 2.212_301_5e-7;
        const ROOT22: f64 = 1.789_167_9e-6;
        const ROOT44: f64 = 7.363_695_3e-9;
        const ROOT54: f64 = 2.176_580_3e-9;
        const ROOT32: f64 = 3.739_379_2e-7;
        const ROOT52: f64 = 1.142_863_9e-7;
        const ZNL: f64 = 1.583_521_8e-4;
        const ZNS: f64 = 1.194_59e-5;
        let nm = self.no_unkozai;
        let em = self.ecco;
        let inclm = self.inclo;
        let (sinim, cosim, emsq) = (c.sinim, c.cosim, c.emsq);
        let mut irez = 0;
        if 0.003_490_658_5 < nm && nm < 0.005_235_987_7 {
            irez = 1;
        }
        if (8.26e-3..=9.24e-3).contains(&nm) && em >= 0.5 {
            irez = 2;
        }
        let ses = c.ss1 * ZNS * c.ss5;
        let sis = c.ss2 * ZNS * (c.sz11 + c.sz13);
        let sls = -ZNS * c.ss3 * (c.sz1 + c.sz3 - 14.0 - 6.0 * emsq);
        let sghs = c.ss4 * ZNS * (c.sz31 + c.sz33 - 6.0);
        let mut shs = -ZNS * c.ss2 * (c.sz21 + c.sz23);
        let near_equator = inclm < 5.235_987_7e-2 || inclm > PI - 5.235_987_7e-2;
        if near_equator {
            shs = 0.0;
        }
        if sinim != 0.0 {
            shs /= sinim;
        }
        let sgs = sghs - cosim * shs;
        let d = &mut self.d;
        d.dedt = ses + c.s1 * ZNL * c.s5;
        d.didt = sis + c.s2 * ZNL * (c.z11 + c.z13);
        d.dmdt = sls - ZNL * c.s3 * (c.z1 + c.z3 - 14.0 - 6.0 * emsq);
        let sghl = c.s4 * ZNL * (c.z31 + c.z33 - 6.0);
        let mut shll = -ZNL * c.s2 * (c.z21 + c.z23);
        if near_equator {
            shll = 0.0;
        }
        d.domdt = sgs + sghl;
        d.dnodt = shs;
        if sinim != 0.0 {
            d.domdt -= cosim / sinim * shll;
            d.dnodt += shll / sinim;
        }
        let theta = self.gsto.rem_euclid(TWOPI);
        d.irez = irez;
        if irez == 0 {
            return;
        }
        let aonv = (nm / self.xke).powf(X2O3);
        if irez == 2 {
            let cosisq = cosim * cosim;
            let em = self.ecco;
            let emsq = eccsq;
            let eoc = em * emsq;
            let g201 = -0.306 - (em - 0.64) * 0.440;
            let (g211, g310, g322, g410, g422, g520);
            if em <= 0.65 {
                g211 = 3.616 - 13.2470 * em + 16.2900 * emsq;
                g310 = -19.302 + 117.3900 * em - 228.4190 * emsq + 156.5910 * eoc;
                g322 = -18.9068 + 109.7927 * em - 214.6334 * emsq + 146.5816 * eoc;
                g410 = -41.122 + 242.6940 * em - 471.0940 * emsq + 313.9530 * eoc;
                g422 = -146.407 + 841.8800 * em - 1629.014 * emsq + 1083.4350 * eoc;
                g520 = -532.114 + 3017.977 * em - 5740.032 * emsq + 3708.2760 * eoc;
            } else {
                g211 = -72.099 + 331.819 * em - 508.738 * emsq + 266.724 * eoc;
                g310 = -346.844 + 1582.851 * em - 2415.925 * emsq + 1246.113 * eoc;
                g322 = -342.585 + 1554.908 * em - 2366.899 * emsq + 1215.972 * eoc;
                g410 = -1052.797 + 4758.686 * em - 7193.992 * emsq + 3651.957 * eoc;
                g422 = -3581.690 + 16178.110 * em - 24462.770 * emsq + 12422.520 * eoc;
                g520 = if em > 0.715 { -5149.66 + 29936.92 * em - 54087.36 * emsq + 31324.56 * eoc } else { 1464.74 - 4664.75 * em + 3763.64 * emsq };
            }
            let (g533, g521, g532) = if em < 0.7 {
                (-919.22770 + 4988.6100 * em - 9064.7700 * emsq + 5542.21 * eoc, -822.71072 + 4568.6173 * em - 8491.4146 * emsq + 5337.524 * eoc, -853.66600 + 4690.2500 * em - 8624.7700 * emsq + 5341.4 * eoc)
            } else {
                (-37995.780 + 161616.52 * em - 229838.20 * emsq + 109377.94 * eoc, -51752.104 + 218913.95 * em - 309468.16 * emsq + 146349.42 * eoc, -40023.880 + 170470.89 * em - 242699.48 * emsq + 115605.82 * eoc)
            };
            let sini2 = sinim * sinim;
            let f220 = 0.75 * (1.0 + 2.0 * cosim + cosisq);
            let f221 = 1.5 * sini2;
            let f321 = 1.875 * sinim * (1.0 - 2.0 * cosim - 3.0 * cosisq);
            let f322 = -1.875 * sinim * (1.0 + 2.0 * cosim - 3.0 * cosisq);
            let f441 = 35.0 * sini2 * f220;
            let f442 = 39.3750 * sini2 * sini2;
            let f522 = 9.84375 * sinim * (sini2 * (1.0 - 2.0 * cosim - 5.0 * cosisq) + 0.33333333 * (-2.0 + 4.0 * cosim + 6.0 * cosisq));
            let f523 = sinim * (4.92187512 * sini2 * (-2.0 - 4.0 * cosim + 10.0 * cosisq) + 6.56250012 * (1.0 + 2.0 * cosim - 3.0 * cosisq));
            let f542 = 29.53125 * sinim * (2.0 - 8.0 * cosim + cosisq * (-12.0 + 8.0 * cosim + 10.0 * cosisq));
            let f543 = 29.53125 * sinim * (-2.0 - 8.0 * cosim + cosisq * (12.0 + 8.0 * cosim - 10.0 * cosisq));
            let xno2 = nm * nm;
            let ainv2 = aonv * aonv;
            let mut temp1 = 3.0 * xno2 * ainv2;
            let mut temp = temp1 * ROOT22;
            d.d2201 = temp * f220 * g201;
            d.d2211 = temp * f221 * g211;
            temp1 *= aonv;
            temp = temp1 * ROOT32;
            d.d3210 = temp * f321 * g310;
            d.d3222 = temp * f322 * g322;
            temp1 *= aonv;
            temp = 2.0 * temp1 * ROOT44;
            d.d4410 = temp * f441 * g410;
            d.d4422 = temp * f442 * g422;
            temp1 *= aonv;
            temp = temp1 * ROOT52;
            d.d5220 = temp * f522 * g520;
            d.d5232 = temp * f523 * g532;
            temp = 2.0 * temp1 * ROOT54;
            d.d5421 = temp * f542 * g521;
            d.d5433 = temp * f543 * g533;
            d.xlamo = (self.mo + self.nodeo + self.nodeo - theta - theta).rem_euclid(TWOPI);
            d.xfact = self.mdot + d.dmdt + 2.0 * (self.nodedot + d.dnodt - RPTIM) - self.no_unkozai;
        } else {
            let g200 = 1.0 + emsq * (-2.5 + 0.8125 * emsq);
            let g310 = 1.0 + 2.0 * emsq;
            let g300 = 1.0 + emsq * (-6.0 + 6.60937 * emsq);
            let f220 = 0.75 * (1.0 + cosim) * (1.0 + cosim);
            let f311 = 0.9375 * sinim * sinim * (1.0 + 3.0 * cosim) - 0.75 * (1.0 + cosim);
            let f330 = 1.875 * (1.0 + cosim).powi(3);
            let del1 = 3.0 * nm * nm * aonv * aonv;
            d.del2 = 2.0 * del1 * f220 * g200 * Q22;
            d.del3 = 3.0 * del1 * f330 * g300 * Q33 * aonv;
            d.del1 = del1 * f311 * g310 * Q31 * aonv;
            d.xlamo = (self.mo + self.nodeo + self.argpo - theta).rem_euclid(TWOPI);
            d.xfact = self.mdot + xpidot - RPTIM + d.dmdt + d.domdt + d.dnodt - self.no_unkozai;
        }
    }

    /// The deep-space secular terms and the resonance's integration from the epoch (`dspace`).
    #[allow(clippy::too_many_arguments)]
    fn dspace(&self, t: f64, mut em: f64, mut argpm: f64, mut inclm: f64, mut mm: f64, mut nodem: f64, mut nm: f64) -> (f64, f64, f64, f64, f64, f64) {
        const FASX2: f64 = 0.131_309_08;
        const FASX4: f64 = 2.884_319_8;
        const FASX6: f64 = 0.374_480_87;
        const G22: f64 = 5.768_639_6;
        const G32: f64 = 0.952_408_98;
        const G44: f64 = 1.801_499_8;
        const G52: f64 = 1.050_833_0;
        const G54: f64 = 4.410_889_8;
        const STEPP: f64 = 720.0;
        const STEPN: f64 = -720.0;
        const STEP2: f64 = 259_200.0;
        let d = &self.d;
        let theta = (self.gsto + t * RPTIM).rem_euclid(TWOPI);
        em += d.dedt * t;
        inclm += d.didt * t;
        argpm += d.domdt * t;
        nodem += d.dnodt * t;
        mm += d.dmdt * t;
        if d.irez == 0 {
            return (em, argpm, inclm, mm, nodem, nm);
        }
        // Integrated from the epoch on every call (the reference keeps no state between calls
        // either), in half-day steps.
        let mut atime = 0.0;
        let mut xni = self.no_unkozai;
        let mut xli = d.xlamo;
        let delt = if t > 0.0 { STEPP } else { STEPN };
        let (mut xndt, mut xldot, mut xnddt);
        let ft;
        loop {
            if d.irez != 2 {
                xndt = d.del1 * (xli - FASX2).sin() + d.del2 * (2.0 * (xli - FASX4)).sin() + d.del3 * (3.0 * (xli - FASX6)).sin();
                xldot = xni + d.xfact;
                xnddt = d.del1 * (xli - FASX2).cos() + 2.0 * d.del2 * (2.0 * (xli - FASX4)).cos() + 3.0 * d.del3 * (3.0 * (xli - FASX6)).cos();
                xnddt *= xldot;
            } else {
                let xomi = self.argpo + self.argpdot * atime;
                let x2omi = xomi + xomi;
                let x2li = xli + xli;
                xndt = d.d2201 * (x2omi + xli - G22).sin()
                    + d.d2211 * (xli - G22).sin()
                    + d.d3210 * (xomi + xli - G32).sin()
                    + d.d3222 * (-xomi + xli - G32).sin()
                    + d.d4410 * (x2omi + x2li - G44).sin()
                    + d.d4422 * (x2li - G44).sin()
                    + d.d5220 * (xomi + xli - G52).sin()
                    + d.d5232 * (-xomi + xli - G52).sin()
                    + d.d5421 * (xomi + x2li - G54).sin()
                    + d.d5433 * (-xomi + x2li - G54).sin();
                xldot = xni + d.xfact;
                xnddt = d.d2201 * (x2omi + xli - G22).cos()
                    + d.d2211 * (xli - G22).cos()
                    + d.d3210 * (xomi + xli - G32).cos()
                    + d.d3222 * (-xomi + xli - G32).cos()
                    + d.d5220 * (xomi + xli - G52).cos()
                    + d.d5232 * (-xomi + xli - G52).cos()
                    + 2.0 * (d.d4410 * (x2omi + x2li - G44).cos() + d.d4422 * (x2li - G44).cos() + d.d5421 * (xomi + x2li - G54).cos() + d.d5433 * (-xomi + x2li - G54).cos());
                xnddt *= xldot;
            }
            if (t - atime).abs() >= STEPP {
                xli += xldot * delt + xndt * STEP2;
                xni += xndt * delt + xnddt * STEP2;
                atime += delt;
            } else {
                ft = t - atime;
                break;
            }
        }
        nm = xni + xndt * ft + xnddt * ft * ft * 0.5;
        let xl = xli + xldot * ft + xndt * ft * ft * 0.5;
        mm = if d.irez != 1 { xl - 2.0 * nodem + 2.0 * theta } else { xl - nodem - argpm + theta };
        (em, argpm, inclm, mm, nodem, nm)
    }

    /// Position and velocity (TEME, km and km/s) `tsince` minutes after the epoch.
    pub fn propagate(&self, tsince: f64) -> Result<StateVector, String> {
        let t = tsince;
        let xmdf = self.mo + self.mdot * t;
        let argpdf = self.argpo + self.argpdot * t;
        let nodedf = self.nodeo + self.nodedot * t;
        let mut argpm = argpdf;
        let mut mm = xmdf;
        let t2 = t * t;
        let mut nodem = nodedf + self.nodecf * t2;
        let mut tempa = 1.0 - self.cc1 * t;
        let mut tempe = self.bstar * self.cc4 * t;
        let mut templ = self.t2cof * t2;
        if !self.isimp {
            let delomg = self.omgcof * t;
            let delm = self.xmcof * ((1.0 + self.eta * xmdf.cos()).powi(3) - self.delmo);
            let temp = delomg + delm;
            mm = xmdf + temp;
            argpm = argpdf - temp;
            let t3 = t2 * t;
            let t4 = t3 * t;
            tempa = tempa - self.d2 * t2 - self.d3 * t3 - self.d4 * t4;
            tempe += self.bstar * self.cc5 * (mm.sin() - self.sinmao);
            templ += self.t3cof * t3 + t4 * (self.t4cof + t * self.t5cof);
        }
        let mut nm = self.no_unkozai;
        let mut em = self.ecco;
        let mut inclm = self.inclo;
        if self.deep {
            (em, argpm, inclm, mm, nodem, nm) = self.dspace(t, em, argpm, inclm, mm, nodem, nm);
        }
        if nm <= 0.0 {
            return Err(format!("mean motion {nm:.6} is not positive"));
        }
        let am = (self.xke / nm).powf(X2O3) * tempa * tempa;
        nm = self.xke / am.powf(1.5);
        em -= tempe;
        if !(-0.001..1.0).contains(&em) {
            return Err(format!("mean eccentricity {em:.6} out of range"));
        }
        let em = em.max(1.0e-6);
        mm += self.no_unkozai * templ;
        let xlm = (mm + argpm + nodem).rem_euclid(TWOPI);
        // (C's fmod: the node keeps its sign.)
        let nodem = nodem % TWOPI;
        let argpm = argpm.rem_euclid(TWOPI);
        let mm = (xlm - argpm - nodem).rem_euclid(TWOPI);

        let (mut ep, mut xincp, mut argpp, mut nodep, mut mp) = (em, inclm, argpm, nodem, mm);
        let (mut sinip, mut cosip) = inclm.sin_cos();
        let (mut aycof, mut xlcof_, mut con41, mut x1mth2, mut x7thm1) = (self.aycof, self.xlcof, self.con41, self.x1mth2, self.x7thm1);
        if self.deep {
            (ep, xincp, nodep, argpp, mp) = dpper(&self.d, t, ep, xincp, nodep, argpp, mp);
            if xincp < 0.0 {
                xincp = -xincp;
                nodep += PI;
                argpp -= PI;
            }
            if !(0.0..=1.0).contains(&ep) {
                return Err(format!("perturbed eccentricity {ep:.6} out of range"));
            }
            (sinip, cosip) = xincp.sin_cos();
            aycof = -0.5 * self.j3oj2 * sinip;
            xlcof_ = xlcof(self.j3oj2, sinip, cosip);
        }
        let axnl = ep * argpp.cos();
        let temp = 1.0 / (am * (1.0 - ep * ep));
        let aynl = ep * argpp.sin() + temp * aycof;
        let xl = mp + argpp + nodep + temp * xlcof_ * axnl;
        // Kepler's equation (with the long-period terms).
        let u = (xl - nodep).rem_euclid(TWOPI);
        let mut eo1 = u;
        let mut tem5: f64 = 9999.9;
        let (mut sineo1, mut coseo1) = (0.0, 0.0);
        let mut ktr = 1;
        while tem5.abs() >= 1.0e-12 && ktr <= 10 {
            (sineo1, coseo1) = eo1.sin_cos();
            tem5 = 1.0 - coseo1 * axnl - sineo1 * aynl;
            tem5 = (u - aynl * coseo1 + axnl * sineo1 - eo1) / tem5;
            if tem5.abs() >= 0.95 {
                tem5 = if tem5 > 0.0 { 0.95 } else { -0.95 };
            }
            eo1 += tem5;
            ktr += 1;
        }
        let ecose = axnl * coseo1 + aynl * sineo1;
        let esine = axnl * sineo1 - aynl * coseo1;
        let el2 = axnl * axnl + aynl * aynl;
        let pl = am * (1.0 - el2);
        if pl < 0.0 {
            return Err(format!("semilatus rectum {pl:.6} is negative"));
        }
        let rl = am * (1.0 - ecose);
        let rdotl = am.sqrt() * esine / rl;
        let rvdotl = pl.sqrt() / rl;
        let betal = (1.0 - el2).sqrt();
        let temp = esine / (1.0 + betal);
        let sinu = am / rl * (sineo1 - aynl - axnl * temp);
        let cosu = am / rl * (coseo1 - axnl + aynl * temp);
        let mut su = sinu.atan2(cosu);
        let sin2u = (cosu + cosu) * sinu;
        let cos2u = 1.0 - 2.0 * sinu * sinu;
        let temp = 1.0 / pl;
        let temp1 = 0.5 * J2 * temp;
        let temp2 = temp1 * temp;
        if self.deep {
            let cosisq = cosip * cosip;
            con41 = 3.0 * cosisq - 1.0;
            x1mth2 = 1.0 - cosisq;
            x7thm1 = 7.0 * cosisq - 1.0;
        }
        let mrt = rl * (1.0 - 1.5 * temp2 * betal * con41) + 0.5 * temp1 * x1mth2 * cos2u;
        su -= 0.25 * temp2 * x7thm1 * sin2u;
        let xnode = nodep + 1.5 * temp2 * cosip * sin2u;
        let xinc = xincp + 1.5 * temp2 * cosip * sinip * cos2u;
        let mvt = rdotl - nm * temp1 * x1mth2 * sin2u / self.xke;
        let rvdot = rvdotl + nm * temp1 * (x1mth2 * cos2u + 1.5 * con41) / self.xke;
        let (sinsu, cossu) = su.sin_cos();
        let (snod, cnod) = xnode.sin_cos();
        let (sini, cosi) = xinc.sin_cos();
        let xmx = -snod * cosi;
        let xmy = cnod * cosi;
        let ux = xmx * sinsu + cnod * cossu;
        let uy = xmy * sinsu + snod * cossu;
        let uz = sini * sinsu;
        let vx = xmx * cossu - cnod * sinsu;
        let vy = xmy * cossu - snod * sinsu;
        let vz = sini * cossu;
        if mrt < 1.0 {
            return Err("the satellite has decayed".to_string());
        }
        let mr = mrt * RE;
        let vkmpersec = RE * self.xke / 60.0;
        Ok(StateVector {
            r: [mr * ux, mr * uy, mr * uz],
            v: [(mvt * ux + rvdot * vx) * vkmpersec, (mvt * uy + rvdot * vy) * vkmpersec, (mvt * uz + rvdot * vz) * vkmpersec],
        })
    }

    /// The state at a moment (seconds since 1970).
    pub fn at_unix(&self, unix: f64) -> Result<StateVector, String> {
        self.propagate((unix - self.epoch_unix) / 60.0)
    }

    /// Where it is over the Earth at a moment.
    pub fn fix(&self, unix: f64) -> Result<Fix, String> {
        let sv = self.at_unix(unix)?;
        let ecef = teme_to_ecef(sv.r, gmst_unix(unix));
        let (lat, lon, alt_km) = geodetic(ecef);
        Ok(Fix { lat, lon, alt_km, speed_kms: norm(sv.v), teme: sv.r })
    }
}

fn xlcof(j3oj2: f64, sinio: f64, cosio: f64) -> f64 {
    let den = if (cosio + 1.0).abs() > 1.5e-12 { 1.0 + cosio } else { 1.5e-12 };
    -0.25 * j3oj2 * sinio * (3.0 + 5.0 * cosio) / den
}

/// The lunar-solar periodics (`dpper` after initialisation), with Lyddane's form near the
/// equator (an inclination under 0.2 rad).
#[allow(clippy::too_many_arguments)]
fn dpper(d: &Deep, t: f64, mut ep: f64, mut inclp: f64, mut nodep: f64, mut argpp: f64, mut mp: f64) -> (f64, f64, f64, f64, f64) {
    const ZNS: f64 = 1.194_59e-5;
    const ZES: f64 = 0.01675;
    const ZNL: f64 = 1.583_521_8e-4;
    const ZEL: f64 = 0.05490;
    let zm = d.zmos + ZNS * t;
    let zf = zm + 2.0 * ZES * zm.sin();
    let sinzf = zf.sin();
    let f2 = 0.5 * sinzf * sinzf - 0.25;
    let f3 = -0.5 * sinzf * zf.cos();
    let ses = d.se2 * f2 + d.se3 * f3;
    let sis = d.si2 * f2 + d.si3 * f3;
    let sls = d.sl2 * f2 + d.sl3 * f3 + d.sl4 * sinzf;
    let sghs = d.sgh2 * f2 + d.sgh3 * f3 + d.sgh4 * sinzf;
    let shs = d.sh2 * f2 + d.sh3 * f3;
    let zm = d.zmol + ZNL * t;
    let zf = zm + 2.0 * ZEL * zm.sin();
    let sinzf = zf.sin();
    let f2 = 0.5 * sinzf * sinzf - 0.25;
    let f3 = -0.5 * sinzf * zf.cos();
    let sel = d.ee2 * f2 + d.e3 * f3;
    let sil = d.xi2 * f2 + d.xi3 * f3;
    let sll = d.xl2 * f2 + d.xl3 * f3 + d.xl4 * sinzf;
    let sghl = d.xgh2 * f2 + d.xgh3 * f3 + d.xgh4 * sinzf;
    let shll = d.xh2 * f2 + d.xh3 * f3;
    let pe = ses + sel;
    let pinc = sis + sil;
    let pl = sls + sll;
    let mut pgh = sghs + sghl;
    let mut ph = shs + shll;
    inclp += pinc;
    ep += pe;
    let (sinip, cosip) = inclp.sin_cos();
    if inclp >= 0.2 {
        ph /= sinip;
        pgh -= cosip * ph;
        argpp += pgh;
        nodep += ph;
        mp += pl;
    } else {
        let (sinop, cosop) = nodep.sin_cos();
        let alfdp = sinip * sinop + (ph * cosop + pinc * cosip * sinop);
        let betdp = sinip * cosop + (-ph * sinop + pinc * cosip * cosop);
        nodep %= TWOPI;
        let xls = mp + argpp + pl + pgh + (cosip - pinc * sinip) * nodep;
        let xnoh = nodep;
        nodep = alfdp.atan2(betdp);
        if (xnoh - nodep).abs() > PI {
            if nodep < xnoh {
                nodep += TWOPI;
            } else {
                nodep -= TWOPI;
            }
        }
        mp += pl;
        argpp = xls - mp - cosip * nodep;
    }
    (ep, inclp, nodep, argpp, mp)
}

// ------------------------------------------------------------------ frames and the Earth ----

/// Greenwich mean sidereal time (IAU-82) in radians for a Julian date (UT1).
pub fn gstime(jdut1: f64) -> f64 {
    let tut1 = (jdut1 - 2_451_545.0) / 36_525.0;
    let temp = -6.2e-6 * tut1 * tut1 * tut1 + 0.093_104 * tut1 * tut1 + (876_600.0 * 3600.0 + 8_640_184.812_866) * tut1 + 67_310.548_41;
    (temp * DEG / 240.0).rem_euclid(TWOPI)
}

/// The Julian date of a moment (seconds since 1970).
pub fn jd_of_unix(unix: f64) -> f64 {
    unix / 86_400.0 + JD_UNIX
}

/// GMST at a moment (seconds since 1970).
pub fn gmst_unix(unix: f64) -> f64 {
    gstime(jd_of_unix(unix))
}

/// TEME to the Earth-fixed frame (the sidereal turn; polar motion left out).
pub fn teme_to_ecef(r: [f64; 3], gmst: f64) -> [f64; 3] {
    let (s, c) = gmst.sin_cos();
    [c * r[0] + s * r[1], -s * r[0] + c * r[1], r[2]]
}

pub fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// WGS-84 latitude and longitude (degrees) and altitude (km) of an Earth-fixed point.
pub fn geodetic(r: [f64; 3]) -> (f64, f64, f64) {
    let e2 = WGS84_F * (2.0 - WGS84_F);
    let lon = r[1].atan2(r[0]);
    let p = (r[0] * r[0] + r[1] * r[1]).sqrt();
    let mut lat = r[2].atan2(p * (1.0 - e2));
    let mut alt = 0.0;
    for _ in 0..6 {
        let s = lat.sin();
        let n = WGS84_A / (1.0 - e2 * s * s).sqrt();
        alt = if lat.cos().abs() > 1e-9 { p / lat.cos() - n } else { r[2].abs() - n * (1.0 - e2) };
        lat = r[2].atan2(p * (1.0 - e2 * n / (n + alt)));
    }
    (lat / DEG, lon / DEG, alt)
}

/// The Earth-fixed position of a place (degrees, km above the ellipsoid).
pub fn ecef_of(lat: f64, lon: f64, alt_km: f64) -> [f64; 3] {
    let e2 = WGS84_F * (2.0 - WGS84_F);
    let (sl, cl) = (lat * DEG).sin_cos();
    let (so, co) = (lon * DEG).sin_cos();
    let n = WGS84_A / (1.0 - e2 * sl * sl).sqrt();
    [(n + alt_km) * cl * co, (n + alt_km) * cl * so, (n * (1.0 - e2) + alt_km) * sl]
}

/// Where someone stands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observer {
    pub lat: f64,
    pub lon: f64,
    pub alt_km: f64,
}

/// Azimuth (degrees from north, clockwise), elevation (degrees) and range (km) of an
/// Earth-fixed point as seen from `o`.
pub fn look_angles(o: &Observer, target: [f64; 3]) -> (f64, f64, f64) {
    let me = ecef_of(o.lat, o.lon, o.alt_km);
    let d = [target[0] - me[0], target[1] - me[1], target[2] - me[2]];
    let (sl, cl) = (o.lat * DEG).sin_cos();
    let (so, co) = (o.lon * DEG).sin_cos();
    let east = -so * d[0] + co * d[1];
    let north = -sl * co * d[0] - sl * so * d[1] + cl * d[2];
    let up = cl * co * d[0] + cl * so * d[1] + sl * d[2];
    let range = norm(d);
    let az = east.atan2(north).rem_euclid(TWOPI) / DEG;
    let el = (up / range).clamp(-1.0, 1.0).asin() / DEG;
    (az, el, range)
}

/// The Sun's position (km, mean equator of date; Vallado's low-precision series, ~0.01°).
pub fn sun_eci(jd: f64) -> [f64; 3] {
    let t = (jd - 2_451_545.0) / 36_525.0;
    let mean_long = (280.460 + 36_000.77 * t).rem_euclid(360.0);
    let mean_anomaly = ((357.529_109_2 + 35_999.050_34 * t).rem_euclid(360.0)) * DEG;
    let ecl_long = (mean_long + 1.914_666_471 * mean_anomaly.sin() + 0.019_994_643 * (2.0 * mean_anomaly).sin()) * DEG;
    let obliquity = (23.439_291 - 0.013_004_2 * t) * DEG;
    let r = (1.000_140_612 - 0.016_708_617 * mean_anomaly.cos() - 0.000_139_589 * (2.0 * mean_anomaly).cos()) * AU_KM;
    [r * ecl_long.cos(), r * obliquity.cos() * ecl_long.sin(), r * obliquity.sin() * ecl_long.sin()]
}

/// The Sun's elevation (degrees) for someone at `o` (the tests check the passes with it).
#[cfg_attr(not(test), allow(dead_code))]
pub fn sun_elevation(o: &Observer, unix: f64) -> f64 {
    let sun = teme_to_ecef(sun_eci(jd_of_unix(unix)), gmst_unix(unix));
    look_angles(o, sun).1
}

/// Whether the Sun shines on a satellite (the Earth's shadow as a cylinder: within a few
/// seconds of the real umbra's edge for a low orbit).
pub fn sunlit(sat_teme: [f64; 3], sun: [f64; 3]) -> bool {
    let n = norm(sun);
    let u = [sun[0] / n, sun[1] / n, sun[2] / n];
    let along = dot(sat_teme, u);
    if along >= 0.0 {
        return true;
    }
    let perp = [sat_teme[0] - along * u[0], sat_teme[1] - along * u[1], sat_teme[2] - along * u[2]];
    norm(perp) > WGS84_A
}

/// A satellite over the Earth at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    pub alt_km: f64,
    /// Orbital speed (km/s).
    pub speed_kms: f64,
    pub teme: [f64; 3],
}

// ------------------------------------------------------------------------------ passes ----

/// The visible part of one pass over an observer.
#[derive(Debug, Clone, PartialEq)]
pub struct Pass {
    /// When it can first be seen (seconds since 1970), its azimuth and elevation then.
    pub start: f64,
    pub start_az: f64,
    pub start_el: f64,
    /// Its highest point.
    pub max_at: f64,
    pub max_az: f64,
    pub max_el: f64,
    /// When it is gone (into the Earth's shadow, below 10°, or the sky turned light).
    pub end: f64,
    pub end_az: f64,
    pub end_el: f64,
}

impl Pass {
    pub fn duration_secs(&self) -> f64 {
        self.end - self.start
    }
}

/// Lowest elevation a satellite counts as seen at (haze and roofs below it).
pub const VISIBLE_EL: f64 = 10.0;
/// The sky is dark enough once the Sun is this far below the horizon (civil twilight's end).
pub const DARK_SUN_EL: f64 = -6.0;

/// Seen from `o` at `unix`: (azimuth, elevation, visible to the eye).
fn sky(sat: &Sgp4, o: &Observer, unix: f64) -> Option<(f64, f64, bool)> {
    let sv = sat.at_unix(unix).ok()?;
    let gmst = gmst_unix(unix);
    let (az, el, _) = look_angles(o, teme_to_ecef(sv.r, gmst));
    if el < VISIBLE_EL {
        return Some((az, el, false));
    }
    let sun = sun_eci(jd_of_unix(unix));
    let sun_el = look_angles(o, teme_to_ecef(sun, gmst)).1;
    Some((az, el, sun_el < DARK_SUN_EL && sunlit(sv.r, sun)))
}

/// The visible passes from `from` (seconds since 1970) over the next `hours`: the satellite
/// lit by the Sun, the sky dark (the Sun 6° below the horizon) and the satellite 10° or more up.
pub fn visible_passes(sat: &Sgp4, o: &Observer, from: f64, hours: f64) -> Vec<Pass> {
    let step = if sat.is_deep_space() { 120.0 } else { 20.0 };
    let end = from + hours * 3600.0;
    let seen = |t: f64| sky(sat, o, t).is_some_and(|s| s.2);
    // The moment between `a` (one way) and `b` (the other) where it changes, to a second.
    let edge = |mut a: f64, mut b: f64| {
        let want = seen(b);
        while (b - a).abs() > 1.0 {
            let m = (a + b) / 2.0;
            if seen(m) == want {
                b = m;
            } else {
                a = m;
            }
        }
        b
    };
    let mut passes = Vec::new();
    let mut t = from;
    let mut was = seen(t);
    let mut start = if was { Some(from) } else { None };
    while t < end {
        let next = (t + step).min(end);
        let now = seen(next);
        match (was, now) {
            (false, true) => start = Some(edge(t, next)),
            (true, false) => {
                if let Some(s) = start.take() {
                    passes.extend(describe(sat, o, s, edge(t, next)));
                }
            }
            _ => {}
        }
        was = now;
        t = next;
    }
    if let Some(s) = start {
        passes.extend(describe(sat, o, s, end));
    }
    passes
}

/// A pass from its visible start and end: the highest point found in 5-second steps.
fn describe(sat: &Sgp4, o: &Observer, start: f64, end: f64) -> Option<Pass> {
    let (start_az, start_el, _) = sky(sat, o, start)?;
    // (`end` is the first moment it is not seen, within a second of the last one that it is:
    // the shadow's edge when it vanishes at its highest point. Its highest point is one seen.)
    let (end_az, end_el, _) = sky(sat, o, end - 1.0)?;
    let mut best = (start, start_az, start_el);
    if end_el > best.2 {
        best = (end - 1.0, end_az, end_el);
    }
    let mut t = start;
    while t < end - 1.0 {
        if let Some((az, el, _)) = sky(sat, o, t) {
            if el > best.2 {
                best = (t, az, el);
            }
        }
        t += 5.0;
    }
    (end - start >= 10.0).then_some(Pass { start, start_az, start_el, max_at: best.0, max_az: best.1, max_el: best.2, end, end_az, end_el })
}

/// The 16 points of the compass: "WSW".
pub fn compass(az: f64) -> &'static str {
    const POINTS: [&str; 16] = ["N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW", "NNW"];
    POINTS[((az.rem_euclid(360.0) + 11.25) / 22.5) as usize % 16]
}

/// The eight directions as said: "south-west".
pub fn compass_word(az: f64) -> &'static str {
    const WORDS: [&str; 8] = ["north", "north-east", "east", "south-east", "south", "south-west", "west", "north-west"];
    WORDS[((az.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8]
}

/// The ground track (lat, lon) from `from_min` to `to_min` minutes around `unix`, a point a
/// minute.
pub fn ground_track(sat: &Sgp4, unix: f64, from_min: i32, to_min: i32) -> Vec<(f64, f64)> {
    (from_min..=to_min).filter_map(|m| sat.fix(unix + m as f64 * 60.0).ok()).map(|f| (f.lat, f.lon)).collect()
}

/// Seconds since 1970 now.
pub fn now_unix() -> f64 {
    let now = chrono::Utc::now();
    now.timestamp() as f64 + now.timestamp_subsec_nanos() as f64 * 1e-9
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A set from the verification file (`SGP4-VER.TLE`) and the reference's own vectors
    /// (`tcppver.out`): minutes, position (km), velocity (km/s).
    fn check(line1: &str, line2: &str, expected: &[(f64, [f64; 3], Option<[f64; 3]>)]) {
        let el = parse_tle("test", line1, line2).expect("a readable set");
        let sat = Sgp4::new(&el).expect("initialised");
        for (t, r, v) in expected {
            let sv = sat.propagate(*t).unwrap_or_else(|e| panic!("t={t}: {e}"));
            for i in 0..3 {
                assert!((sv.r[i] - r[i]).abs() < 1e-3, "t={t} r[{i}] {} vs {}", sv.r[i], r[i]);
                if let Some(v) = v {
                    assert!((sv.v[i] - v[i]).abs() < 1e-6, "t={t} v[{i}] {} vs {}", sv.v[i], v[i]);
                }
            }
        }
    }

    #[test]
    fn near_earth_vectors_of_the_verification_file() {
        // 00005 (Vanguard 1): eccentric, near-earth.
        check(
            "1 00005U 58002B   00179.78495062  .00000023  00000-0  28098-4 0  4753",
            "2 00005  34.2682 348.7242 1859667 331.7664  19.3264 10.82419157413667",
            &[
                (0.0, [7022.46529266, -1400.08296755, 0.03995155], Some([1.893841015, 6.405893759, 4.534807250])),
                (360.0, [-7154.03120202, -3783.17682504, -3536.19412294], Some([4.741887409, -4.151817765, -2.093935425])),
                (720.0, [-7134.59340119, 6531.68641334, 3260.27186483], None),
                (1080.0, [5568.53901181, 4492.06992591, 3863.87641983], None),
                (1440.0, [-938.55923943, -6268.18748831, -4294.02924751], Some([7.536105209, -0.427127707, 0.989878080])),
            ],
        );
        // 06251 (Delta 1 debris): low perigee, the drag terms.
        check(
            "1 06251U 62025E   06176.82412014  .00008885  00000-0  12808-3 0  3985",
            "2 06251  58.0579  54.0425 0030035 139.1568 221.1854 15.56387291  6774",
            &[(0.0, [3988.31022699, 5498.96657235, 0.90055879], Some([-3.290032738, 2.357652820, 6.496623475]))],
        );
    }

    #[test]
    fn deep_space_vectors_of_the_verification_file() {
        // 08195 (Molniya 2-14): the 12-hour resonance.
        let molniya = Sgp4::new(&parse_tle("", "1 08195U 75081A   06176.33215444  .00000099  00000-0  11873-3 0   813", "2 08195  64.1586 279.0717 6877146 264.7651  20.2257  2.00491383225656").unwrap()).unwrap();
        assert!(molniya.is_deep_space());
        assert_eq!(molniya.d.irez, 2);
        check(
            "1 08195U 75081A   06176.33215444  .00000099  00000-0  11873-3 0   813",
            "2 08195  64.1586 279.0717 6877146 264.7651  20.2257  2.00491383225656",
            &[
                (0.0, [2349.89483350, -14785.93811562, 0.02119378], Some([2.721488096, -3.256811655, 4.498416672])),
                (120.0, [15223.91713658, -17852.95881713, 25280.39558224], Some([1.079041732, 0.875187372, 2.485682813])),
                (720.0, [2622.13222207, -15125.15464924, 474.51048398], Some([2.688287199, -3.078426664, 4.494979530])),
                (1440.0, [2890.80638268, -15446.43952300, 948.77010176], Some([2.654407490, -2.909344895, 4.486437362])),
            ],
        );
        // 09880 (Molniya 2-17).
        check(
            "1 09880U 77021A   06176.56157475  .00000421  00000-0  10000-3 0  9814",
            "2 09880  64.5968 349.3786 7069051 270.0229  16.3320  2.00813614112380",
            &[(0.0, [13020.06750784, -2449.07193500, 1.15896030], Some([4.247363935, 1.597178501, 4.956708611])), (600.0, [-17069.78000550, 9944.86797897, 13885.91649059], Some([0.044133354, -1.853448464, -3.815303117]))],
        );
        // 14128 (EUTELSAT 1-F1): geosynchronous, the one-day resonance.
        let geo = Sgp4::new(&parse_tle("", "1 14128U 83058A   06176.02844893 -.00000158  00000-0  10000-3 0  9627", "2 14128  11.4384  35.2134 0011562  26.4582 333.5652  0.98870114 46093").unwrap()).unwrap();
        assert_eq!(geo.d.irez, 1);
        check(
            "1 14128U 83058A   06176.02844893 -.00000158  00000-0  10000-3 0  9627",
            "2 14128  11.4384  35.2134 0011562  26.4582 333.5652  0.98870114 46093",
            &[
                (0.0, [34747.57932696, 24502.37114079, -1.32832986], Some([-1.731642662, 2.452772615, 0.608510081])),
                (720.0, [-35597.57919549, -23407.91145393, 282.09554383], Some([1.641405246, -2.506773678, -0.606963478])),
                (1680.0, [-23.77224182, 41945.51688402, 6950.29891751], Some([-3.043358385, -0.057417440, 0.346112094])),
            ],
        );
        // 04632 and 09998: deep space, propagated backwards.
        check(
            "1 04632U 70093B   04031.91070959 -.00000084  00000-0  10000-3 0  9955",
            "2 04632  11.4628 273.1101 1450506 207.6000 143.9350  1.20231981 44145",
            &[(0.0, [2334.11450085, -41920.44035349, -0.03867437], Some([2.826321032, -0.065091664, 0.570936053])), (-5184.0, [-29020.02587128, 13819.84419063, -5713.33679183], Some([-1.768068390, -3.235371192, -0.395206135]))],
        );
        check(
            "1 09998U 74033F   05148.79417928 -.00000112  00000-0  00000+0 0  4480",
            "2 09998   9.4958 313.1750 0270971 327.5225  30.8097  1.16186785 45878",
            &[(-1440.0, [-11362.18265118, -35117.55867813, -5413.62537994], Some([3.137861261, -1.011678260, 0.267510059])), (-1260.0, [22400.45329336, -29798.63236321, -677.91515122], Some([2.638533344, 1.922477736, 0.542792913]))],
        );
        // 11801: a deep-space set with no epoch year's designator and heavy drag.
        check(
            "1 11801U          80230.29629788  .01431103  00000-0  14311-1      13",
            "2 11801  46.7916 230.4354 7318036  47.4722  10.4117  2.28537848    13",
            &[(0.0, [7473.37102491, 428.94748312, 5828.74846783], Some([5.107155391, 6.444680305, -0.186133297])), (1440.0, [9787.87836256, 33753.32249667, -15030.79874625], Some([-1.094251553, 0.923589906, -1.522311008]))],
        );
    }

    #[test]
    fn the_iss_where_an_independent_tracker_put_it() {
        // wheretheiss.at's set and its own position for 2026-11-08 23:16:13 UTC: 46.5219°N
        // 30.2974°E, 425.2 km, 27 581 km/h, in the Earth's shadow, the Sun over 16.79°S 186.89°E.
        let el = parse_tle("ISS (ZARYA)", "1 25544U 98067A   26311.46585591  .00014112  00000+0  26146-3 0  9996", "2 25544  51.6322 311.0744 0007266 358.6454   1.4513 15.49241650593960").unwrap();
        let sat = Sgp4::new(&el).unwrap();
        let at = 1_794_179_773.0;
        let f = sat.fix(at).unwrap();
        assert!((f.lat - 46.5219).abs() < 0.05 && (f.lon - 30.2974).abs() < 0.05, "{f:?}");
        assert!((f.alt_km - 425.23).abs() < 1.5, "{f:?}");
        assert!((f.speed_kms * 3600.0 - 27_580.6).abs() < 30.0, "{f:?}");
        let sun = sun_eci(jd_of_unix(at));
        assert!(!sunlit(f.teme, sun), "the tracker said: eclipsed");
        let sub_sun = geodetic(teme_to_ecef(sun, gmst_unix(at)));
        assert!((sub_sun.0 - -16.7865).abs() < 0.05, "{sub_sun:?}");
        assert!(((sub_sun.1 - 186.8868 + 540.0).rem_euclid(360.0) - 180.0).abs() < 0.1, "{sub_sun:?}");
        // CelesTrak's newer set (OMM JSON) agrees within a few kilometres.
        let omm = json!({"OBJECT_NAME":"ISS (ZARYA)","OBJECT_ID":"1998-067A","EPOCH":"2026-11-08T04:12:35.430048","MEAN_MOTION":15.49256954,"ECCENTRICITY":0.0007219,"INCLINATION":51.632,"RA_OF_ASC_NODE":307.5621,"ARG_OF_PERICENTER":1.6306,"MEAN_ANOMALY":358.4705,"NORAD_CAT_ID":25544,"BSTAR":0.00023266166,"MEAN_MOTION_DOT":0.00012511,"MEAN_MOTION_DDOT":0});
        let newer = Sgp4::new(&parse_omm_record(&omm).unwrap()).unwrap().fix(at).unwrap();
        assert!((newer.lat - f.lat).abs() < 0.3 && ((newer.lon - f.lon + 540.0).rem_euclid(360.0) - 180.0).abs() < 0.3, "{newer:?} vs {f:?}");
    }

    #[test]
    fn element_sets_read_from_json_and_two_lines() {
        let list = parse_omm(&json!([
            {"OBJECT_NAME":"HST","OBJECT_ID":"1990-037B","EPOCH":"2026-11-08T10:00:00.000","MEAN_MOTION":"15.29","ECCENTRICITY":0.0002,"INCLINATION":28.47,"RA_OF_ASC_NODE":10.0,"ARG_OF_PERICENTER":20.0,"MEAN_ANOMALY":30.0,"NORAD_CAT_ID":20580,"BSTAR":0.0001},
            {"OBJECT_NAME":"BROKEN","EPOCH":"never"}
        ]));
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].norad, list[0].name.as_str()), (20580, "HST"));
        assert!((list[0].period_min() - 94.18).abs() < 0.05);
        assert_eq!(parse_epoch("1970-01-02T00:00:00"), Some(86_400.0));
        let close = |field: &str, want: f64| assumed_decimal(field).is_some_and(|v| (v - want).abs() <= want.abs() * 1e-12);
        assert!(close(" 11873-3", 0.11873e-3) && close("-11606-4", -0.11606e-4) && close(" 00000+0", 0.0));
        let tle = parse_tle("X", "1 00005U 58002B   00179.78495062  .00000023  00000-0  28098-4 0  4753", "2 00005  34.2682 348.7242 1859667 331.7664  19.3264 10.82419157413667").unwrap();
        assert_eq!(tle.norad, 5);
        assert!((tle.eccentricity - 0.1859667).abs() < 1e-12);
        // 2000, day 179.78495062.
        let jan1_2000 = 946_684_800.0;
        assert!((tle.epoch_unix - (jan1_2000 + 178.784_950_62 * 86_400.0)).abs() < 1e-3);
        assert_eq!(parse_tle("X", "garbage", "more garbage"), None);
    }

    #[test]
    fn frames_angles_and_the_compass() {
        // A point on the equator at Greenwich, straight up from someone standing there.
        let o = Observer { lat: 0.0, lon: 0.0, alt_km: 0.0 };
        let (_, el, range) = look_angles(&o, ecef_of(0.0, 0.0, 500.0));
        assert!((el - 90.0).abs() < 1e-6 && (range - 500.0).abs() < 1e-6);
        // North-east and low, then due west.
        let (az, el, _) = look_angles(&o, ecef_of(5.0, 5.0, 400.0));
        assert!((az - 45.0).abs() < 1.0 && el > 0.0 && el < 60.0, "{az} {el}");
        assert!((look_angles(&o, ecef_of(0.0, -5.0, 400.0)).0 - 270.0).abs() < 0.01);
        let (lat, lon, alt) = geodetic(ecef_of(48.78, 9.18, 0.25));
        assert!((lat - 48.78).abs() < 1e-9 && (lon - 9.18).abs() < 1e-9 && (alt - 0.25).abs() < 1e-6);
        assert_eq!((compass(0.0), compass(250.0), compass(349.0), compass(359.0)), ("N", "WSW", "N", "N"));
        assert_eq!((compass_word(225.0), compass_word(100.0)), ("south-west", "east"));
        // GMST at J2000.0 is 280.46°.
        assert!((gstime(2_451_545.0) / DEG - 280.460_618_4).abs() < 1e-6);
    }

    #[test]
    fn passes_are_lit_up_in_a_dark_sky() {
        // CelesTrak's set of 2026-11-08 04:12 UTC, and the visible passes heavens-above.com listed
        // on 9 Nov 2026 for Stuttgart (48.78°N 9.18°E, 300 m): none until 11 Nov 17:33:47 UTC;
        // 15 Nov 17:34:14 UTC (10° WSW) up to 68° at 17:37:32, into the shadow at 17:37:46;
        // 18 Nov 16:47:15 UTC up to 89° at 16:50:36.
        let omm = json!({"OBJECT_NAME":"ISS (ZARYA)","OBJECT_ID":"1998-067A","EPOCH":"2026-11-08T04:12:35.430048","MEAN_MOTION":15.49256954,"ECCENTRICITY":0.0007219,"INCLINATION":51.632,"RA_OF_ASC_NODE":307.5621,"ARG_OF_PERICENTER":1.6306,"MEAN_ANOMALY":358.4705,"NORAD_CAT_ID":25544,"BSTAR":0.00023266166});
        let el = parse_omm_record(&omm).unwrap();
        let sat = Sgp4::new(&el).unwrap();
        let o = Observer { lat: 48.78, lon: 9.18, alt_km: 0.3 };
        let utc = |d: u32, h: u32, m: u32, s: u32| chrono::NaiveDate::from_ymd_opt(2026, 11, d).unwrap().and_hms_opt(h, m, s).unwrap().and_utc().timestamp() as f64;
        let passes = visible_passes(&sat, &o, el.epoch_unix, 11.0 * 24.0);
        let listed = |p: &[Pass]| p.iter().map(|p| format!("{} max {:.0}°", chrono::DateTime::from_timestamp(p.start as i64, 0).unwrap(), p.max_el)).collect::<Vec<_>>();
        assert!(passes.iter().all(|p| p.start > utc(11, 12, 0, 0)), "nothing is visible before 11 Nov: {:?}", listed(&passes));
        let near = |start: f64| passes.iter().find(|p| (p.start - start).abs() < 150.0);
        let high = near(utc(15, 17, 34, 14)).unwrap_or_else(|| panic!("the 15 Nov pass: {:?}", listed(&passes)));
        assert!((high.max_el - 68.0).abs() < 5.0 && (high.max_at - utc(15, 17, 37, 32)).abs() < 150.0 && (high.end - utc(15, 17, 37, 46)).abs() < 150.0, "{high:?}");
        let overhead = near(utc(18, 16, 47, 15)).unwrap_or_else(|| panic!("the 18 Nov pass: {:?}", listed(&passes)));
        assert!(overhead.max_el > 80.0, "{overhead:?}");
        for p in &passes {
            assert!(p.max_el >= VISIBLE_EL && p.start < p.max_at + 1.0 && p.max_at <= p.end + 1.0, "{p:?}");
            assert!(p.duration_secs() > 0.0 && p.duration_secs() < 15.0 * 60.0, "{p:?}");
            // Every moment of it: dark sky, lit satellite, high enough.
            for t in [p.start + 1.0, p.max_at, p.end - 1.5] {
                assert!(sun_elevation(&o, t) < DARK_SUN_EL + 0.1, "{p:?}");
                let sv = sat.at_unix(t).unwrap();
                assert!(sunlit(sv.r, sun_eci(jd_of_unix(t))), "{p:?}");
            }
        }
    }
}
