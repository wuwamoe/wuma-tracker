export default interface GlobalState {
  procState: number,
  serverState: number,
  connectionUrl?: string,
  externalConnectionCode?: string,
  peerCount?: number,
}