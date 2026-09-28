/** A microphone the user can pick. */
export type Mic = { id: string; label: string };

/**
 * The audio inputs the system reports. Browsers withhold device labels until
 * microphone permission has been granted once, so before that the entries get
 * a numbered stand-in name rather than an empty one. Returns [] when the
 * device list is not available at all (no permission model, tests).
 */
export async function listMics(): Promise<Mic[]> {
  const md = typeof navigator === "undefined" ? undefined : navigator.mediaDevices;
  if (!md?.enumerateDevices) return [];
  const devices = await md.enumerateDevices();
  const inputs = devices.filter((d) => d.kind === "audioinput" && d.deviceId !== "");
  return inputs.map((d, i) => ({ id: d.deviceId, label: d.label || `Microphone ${i + 1}` }));
}
