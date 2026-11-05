/* Standalone validation only; link the pinned external Swiss toolkit. */
#include <stdio.h>
#include "swephexp.h"

int main(void) {
  const int years[] = {1900, 1950, 1955, 1965, 1971, 1972, 2000};
  /* Matched NASA polynomial/USNO January 1 anchors, not Swiss's default delta-T. */
  const double delta_t[] = {-2.79, 29.07, 31.07, 35.738, 41.170, 42.227, 63.8285};
  char version[256], error[256];
  printf("{\"swisseph_version\":\"%s\",\"rows\":[", swe_version(version));
  for (unsigned int i = 0; i < sizeof(years)/sizeof(years[0]); i++) {
    double jd[2], sun[6], cusps[13], angles[10];
    swe_set_delta_t_userdef(delta_t[i] / 86400.0);
    if (swe_utc_to_jd(years[i], 1, 1, 0, 0, 0.0, SE_GREG_CAL, jd, error) < 0) return 2;
    const int flags = SEFLG_MOSEPH | SEFLG_EQUATORIAL;
    const int sun_flags = swe_calc(jd[0], SE_SUN, flags, sun, error);
    if (sun_flags < 0) return 3;
    if (swe_houses_ex(jd[1], 0, 0.0, 0.0, 'E', cusps, angles) < 0) return 4;
    printf("%s{\"year\":%d,\"delta_t_seconds\":%.9f,\"jd_tt\":%.15f,\"jd_ut1\":%.15f,\"gast_degrees\":%.15f,\"sun_flags\":%d,\"sun_ra_degrees\":%.15f,\"ascendant_degrees\":%.15f,\"midheaven_degrees\":%.15f}",
      i ? "," : "", years[i], delta_t[i], jd[0], jd[1], swe_sidtime(jd[1])*15.0,
      sun_flags, sun[0], angles[0], angles[1]);
  }
  printf("]}\n");
  swe_close();
  return 0;
}
