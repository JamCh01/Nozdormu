use std::net::IpAddr;
use std::path::Path;

/// GeoIP information resolved for a single IP address.
#[derive(Debug, Clone, Default)]
pub struct GeoInfo {
    pub country_code: Option<String>,
    pub country_name: Option<String>,
    pub continent_code: Option<String>,
    pub subdivision_code: Option<String>,
    pub subdivision_name: Option<String>,
    pub city_name: Option<String>,
    pub asn: Option<u32>,
    pub asn_org: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

/// Holds MaxMind GeoIP2 database readers.
/// Supports loading City, Country, and ASN databases independently.
pub struct GeoIpDb {
    city: Option<maxminddb::Reader<Vec<u8>>>,
    country: Option<maxminddb::Reader<Vec<u8>>>,
    asn: Option<maxminddb::Reader<Vec<u8>>>,
}

impl GeoIpDb {
    /// Load databases from a directory.
    /// Expects files named: GeoLite2-City.mmdb, GeoLite2-Country.mmdb, GeoLite2-ASN.mmdb
    /// Missing files are silently skipped.
    pub fn load_from_dir(dir: &Path) -> Self {
        let city = Self::open_db(dir.join("GeoLite2-City.mmdb"));
        let country = Self::open_db(dir.join("GeoLite2-Country.mmdb"));
        let asn = Self::open_db(dir.join("GeoLite2-ASN.mmdb"));

        let loaded: Vec<&str> = [
            city.as_ref().map(|_| "City"),
            country.as_ref().map(|_| "Country"),
            asn.as_ref().map(|_| "ASN"),
        ]
        .into_iter()
        .flatten()
        .collect();

        if loaded.is_empty() {
            log::warn!("[GeoIP] no databases loaded from {}", dir.display());
        } else {
            log::info!("[GeoIP] loaded databases: {}", loaded.join(", "));
        }

        Self { city, country, asn }
    }

    /// Create an empty GeoIpDb (no databases loaded).
    pub fn empty() -> Self {
        Self {
            city: None,
            country: None,
            asn: None,
        }
    }

    /// Returns true if at least one database is loaded.
    pub fn is_available(&self) -> bool {
        self.city.is_some() || self.country.is_some() || self.asn.is_some()
    }

    /// Lookup GeoIP information for an IP address.
    /// Queries all available databases and merges results.
    pub fn lookup(&self, ip: IpAddr) -> GeoInfo {
        let mut info = GeoInfo::default();

        // City database (most detailed — includes country + subdivision + city)
        if let Some(reader) = &self.city {
            if let Ok(city) = reader.lookup::<maxminddb::geoip2::City>(ip) {
                if let Some(country) = &city.country {
                    info.country_code = country.iso_code.map(|s| s.to_string());
                    info.country_name = country
                        .names
                        .as_ref()
                        .and_then(|n| n.get("en"))
                        .map(|s| s.to_string());
                }
                if let Some(continent) = &city.continent {
                    info.continent_code = continent.code.map(|s| s.to_string());
                }
                if let Some(subdivisions) = &city.subdivisions {
                    if let Some(sub) = subdivisions.first() {
                        info.subdivision_code = sub.iso_code.map(|s| s.to_string());
                        info.subdivision_name = sub
                            .names
                            .as_ref()
                            .and_then(|n| n.get("en"))
                            .map(|s| s.to_string());
                    }
                }
                if let Some(city_rec) = &city.city {
                    info.city_name = city_rec
                        .names
                        .as_ref()
                        .and_then(|n| n.get("en"))
                        .map(|s| s.to_string());
                }
                if let Some(location) = &city.location {
                    info.latitude = location.latitude;
                    info.longitude = location.longitude;
                }
            }
        }
        // Fallback: Country database (if City didn't provide country info)
        else if let Some(reader) = &self.country {
            if let Ok(country) = reader.lookup::<maxminddb::geoip2::Country>(ip) {
                if let Some(c) = &country.country {
                    info.country_code = c.iso_code.map(|s| s.to_string());
                    info.country_name = c
                        .names
                        .as_ref()
                        .and_then(|n| n.get("en"))
                        .map(|s| s.to_string());
                }
                if let Some(continent) = &country.continent {
                    info.continent_code = continent.code.map(|s| s.to_string());
                }
            }
        }

        // ASN database
        if let Some(reader) = &self.asn {
            if let Ok(asn) = reader.lookup::<maxminddb::geoip2::Asn>(ip) {
                info.asn = asn.autonomous_system_number;
                info.asn_org = asn.autonomous_system_organization.map(|s| s.to_string());
            }
        }

        info
    }

    fn open_db<P: AsRef<Path>>(path: P) -> Option<maxminddb::Reader<Vec<u8>>> {
        let path = path.as_ref();
        match maxminddb::Reader::open_readfile(path) {
            Ok(reader) => {
                log::info!("[GeoIP] loaded {}", path.display());
                Some(reader)
            }
            Err(e) => {
                log::debug!("[GeoIP] skipped {} ({})", path.display(), e);
                None
            }
        }
    }
}
