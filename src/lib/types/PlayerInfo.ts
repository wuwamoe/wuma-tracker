export default interface PlayerInfo {
  x: number,
  y: number,
  z: number,
  pitch: number,
  yaw: number,
  roll: number,
  /** Loaded world's short name (e.g. `AkiWorld_WP`); absent when unknown. */
  world?: string,
}