/** Whether this Mac keeps recorded audio. Mirrors `sessions::retain_audio`;
 *  `audioGateVectors.json` pins both. */
export function retainsAudio(keepSetting: string | null | undefined, forbidden: boolean): boolean {
  return !forbidden && keepSetting === "true";
}
