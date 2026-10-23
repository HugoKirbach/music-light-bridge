//! Flux Entertainment : paquets HueStream v2, chiffrés en DTLS 1.2 (PSK),
//! envoyés au port UDP 2100 du bridge.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::net::UdpSocket;
use webrtc_dtls::cipher_suite::CipherSuiteId;
use webrtc_dtls::config::Config;
use webrtc_dtls::conn::DTLSConn;

use super::api::{self, EntertainmentArea};
use super::snapshot::{Restored, Snapshot};
use crate::config::BridgeCredentials;

const PORT: u16 = 2100;
/// Hue recommande 50 à 60 paquets/s : le bridge relaie à ~25 Hz en Zigbee
/// et la redondance absorbe les pertes UDP.
pub const FRAME_PERIOD: Duration = Duration::from_micros(16_667);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const AREA_ID_LEN: usize = 36;
const HEADER_LEN: usize = 16 + AREA_ID_LEN;
const SEQUENCE_OFFSET: usize = 11;
const MAX_CHANNELS: usize = 20;

/// Couleur d'un canal en RGB 16 bits ; le bridge la ramène dans le gamut de l'ampoule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u16,
    pub g: u16,
    pub b: u16,
}

impl Rgb {
    pub const OFF: Self = Self { r: 0, g: 0, b: 0 };
    pub const WHITE: Self = Self { r: u16::MAX, g: u16::MAX, b: u16::MAX };

    /// Teinte, saturation et luminosité dans [0, 1] (la teinte boucle).
    pub fn from_hsv(h: f32, s: f32, v: f32) -> Self {
        let h = h.rem_euclid(1.0) * 6.0;
        let c = v * s;
        let x = c * (1.0 - (h % 2.0 - 1.0).abs());
        let (r, g, b) = match h as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = v - c;
        let quantize = |u: f32| ((u + m).clamp(0.0, 1.0) * f32::from(u16::MAX)) as u16;
        Self { r: quantize(r), g: quantize(g), b: quantize(b) }
    }
}

pub struct HueStream {
    conn: DTLSConn,
    bridge: BridgeCredentials,
    area_id: String,
    packet: Vec<u8>,
    /// État des lampes avant le flux, rétabli à la fermeture.
    snapshot: Snapshot,
    /// La photo vient d'une session interrompue, et non de l'état actuel des lampes.
    pub resumed: bool,
}

impl HueStream {
    /// Photographie l'état des lampes, active le streaming sur la zone puis ouvre la session DTLS.
    pub async fn open(bridge: &BridgeCredentials, area: &EntertainmentArea) -> Result<Self> {
        if area.id.len() != AREA_ID_LEN {
            bail!("identifiant de zone inattendu : {}", area.id);
        }
        let (snapshot, resumed) = Snapshot::take(bridge, area).await.context("photo de l'état des lampes")?;
        let opened = match api::set_streaming(bridge, &area.id, true).await {
            Ok(()) => connect(bridge).await,
            Err(e) => Err(e),
        };
        let conn = match opened {
            Ok(conn) => conn,
            Err(e) => {
                // Le flux n'a rien envoyé : les lampes n'ont pas bougé, la photo est inutile.
                let _ = api::set_streaming(bridge, &area.id, false).await;
                if !resumed {
                    let _ = Snapshot::discard();
                }
                return Err(e);
            }
        };
        Ok(Self { conn, bridge: bridge.clone(), area_id: area.id.clone(), packet: header(&area.id), snapshot, resumed })
    }

    pub async fn send(&mut self, channels: &[(u8, Rgb)]) -> Result<()> {
        encode(&mut self.packet, channels);
        self.conn.write(&self.packet, None).await.context("envoi DTLS")?;
        Ok(())
    }

    /// Rouvre le flux après une coupure (bridge redémarré, Wi-Fi perdu, streaming repris par une
    /// autre app) : réactive le streaming de la zone et refait la session DTLS. La photo des
    /// lampes prise à l'ouverture reste celle qui sera rétablie.
    pub async fn reopen(&mut self) -> Result<()> {
        let _ = self.conn.close().await;
        api::set_streaming(&self.bridge, &self.area_id, true).await?;
        self.conn = connect(&self.bridge).await?;
        Ok(())
    }

    /// Ferme la session DTLS, rend la main à l'app Hue et rétablit l'état d'avant le flux.
    pub async fn close(self) -> Result<Restored> {
        let _ = self.conn.close().await;
        api::set_streaming(&self.bridge, &self.area_id, false).await?;
        self.snapshot
            .restore(&self.bridge)
            .await
            .context("état des lampes non rétabli, lance `dj-bridge restore`")
    }
}

/// En-tête HueStream v2, réutilisé pour tous les paquets de la session.
fn header(area_id: &str) -> Vec<u8> {
    let mut packet = Vec::with_capacity(HEADER_LEN + 7 * MAX_CHANNELS);
    packet.extend_from_slice(b"HueStream");
    packet.extend_from_slice(&[0x02, 0x00]); // version 2.0
    packet.push(0x00); // numéro de séquence (ignoré par le bridge)
    packet.extend_from_slice(&[0x00, 0x00]); // réservé
    packet.push(0x00); // espace couleur : RGB
    packet.push(0x00); // réservé
    packet.extend_from_slice(area_id.as_bytes());
    debug_assert_eq!(packet.len(), HEADER_LEN);
    packet
}

/// Remplace les canaux du paquet : identifiant puis R, G, B en 16 bits gros-boutiste.
fn encode(packet: &mut Vec<u8>, channels: &[(u8, Rgb)]) {
    packet.truncate(HEADER_LEN);
    packet[SEQUENCE_OFFSET] = packet[SEQUENCE_OFFSET].wrapping_add(1);
    for &(id, color) in channels.iter().take(MAX_CHANNELS) {
        packet.push(id);
        for value in [color.r, color.g, color.b] {
            packet.extend_from_slice(&value.to_be_bytes());
        }
    }
}

async fn connect(bridge: &BridgeCredentials) -> Result<DTLSConn> {
    let psk = hex::decode(&bridge.client_key).context("clientkey invalide")?;
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect((bridge.ip.as_str(), PORT)).await?;
    let config = Config {
        // Identité PSK = clé d'application ; secret = clientkey décodée.
        psk: Some(Arc::new(
            move |_hint: &[u8]| -> Result<Vec<u8>, webrtc_dtls::Error> { Ok(psk.clone()) },
        )),
        psk_identity_hint: Some(bridge.app_key.as_bytes().to_vec()),
        cipher_suites: vec![CipherSuiteId::Tls_Psk_With_Aes_128_Gcm_Sha256],
        ..Default::default()
    };
    let handshake = DTLSConn::new(Arc::new(socket), config, true, None);
    tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
        .await
        .context("handshake DTLS : pas de réponse du bridge")?
        .context("handshake DTLS refusé")
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: &str = "1a8d99cc-967b-44f2-9202-43f976c0fa6b";

    #[test]
    fn header_follows_huestream_v2_layout() {
        let packet = header(AREA);
        assert_eq!(packet.len(), 52);
        assert_eq!(&packet[..9], b"HueStream");
        assert_eq!(&packet[9..11], &[0x02, 0x00]);
        assert_eq!(packet[14], 0x00, "espace couleur RGB");
        assert_eq!(&packet[16..], AREA.as_bytes());
    }

    #[test]
    fn channels_are_big_endian_and_replace_previous_ones() {
        let mut packet = header(AREA);
        encode(&mut packet, &[(0, Rgb { r: 0x1234, g: 0xABCD, b: 0x00FF }), (3, Rgb::WHITE)]);
        assert_eq!(&packet[52..], &[0, 0x12, 0x34, 0xAB, 0xCD, 0x00, 0xFF, 3, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(packet[SEQUENCE_OFFSET], 1);

        encode(&mut packet, &[(0, Rgb::OFF)]);
        assert_eq!(&packet[52..], &[0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(packet[SEQUENCE_OFFSET], 2);
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(Rgb::from_hsv(0.0, 1.0, 1.0), Rgb { r: u16::MAX, g: 0, b: 0 });
        assert_eq!(Rgb::from_hsv(1.0 / 3.0, 1.0, 1.0), Rgb { r: 0, g: u16::MAX, b: 0 });
        assert_eq!(Rgb::from_hsv(2.0 / 3.0, 1.0, 1.0), Rgb { r: 0, g: 0, b: u16::MAX });
        assert_eq!(Rgb::from_hsv(0.5, 0.0, 1.0), Rgb::WHITE);
        assert_eq!(Rgb::from_hsv(1.25, 1.0, 0.0), Rgb::OFF);
    }
}
