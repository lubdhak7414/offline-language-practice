/**
 * The timezone offset every stats/caps command needs.
 *
 * The backend's `tz_offset_minutes` is "minutes to ADD to UTC to get local
 * time" (UTC-5 -> -300), which is the *opposite* sign convention from
 * `Date.getTimezoneOffset()` (which returns minutes to add to local time to
 * get UTC). Negating here, in one place, means every call site sends the
 * right sign without having to remember why.
 */
export function tzOffsetMinutes(): number {
  return -new Date().getTimezoneOffset();
}
