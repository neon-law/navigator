/* Standalone validation probe; link only against the pinned external Swiss toolkit. */
#include <stdio.h>
#include <math.h>
#include "swephexp.h"

int main(void) {
  char version[256], error[256];
  const double tt = 2451545.0;
  printf("{\"swisseph_version\":\"%s\",\"jd_tt\":%.9f,\"requested_flags\":%d,\"rows\":[", swe_version(version), tt, SEFLG_MOSEPH|SEFLG_SPEED);
  for (int body = 0; body <= 9; body++) {
    double state[6], before[6], after[6], equatorial[6];
    int center_flags = swe_calc(tt, body, SEFLG_MOSEPH|SEFLG_SPEED, state, error);
    int before_flags = swe_calc(tt-30.0/86400.0, body, SEFLG_MOSEPH, before, error);
    int after_flags = swe_calc(tt+30.0/86400.0, body, SEFLG_MOSEPH, after, error);
    int equatorial_flags = swe_calc(tt, body, SEFLG_MOSEPH|SEFLG_SPEED|SEFLG_EQUATORIAL, equatorial, error);
    if (center_flags < 0 || before_flags < 0 || after_flags < 0 || equatorial_flags < 0) return 2;
    printf("%s{\"swiss_body_id\":%d,\"returned_flags\":%d,\"before_flags\":%d,\"after_flags\":%d,\"equatorial_flags\":%d,\"longitude_deg\":%.15f,\"latitude_deg\":%.15f,\"right_ascension_deg\":%.15f,\"declination_deg\":%.15f,\"distance_au\":%.15f,\"reported_longitude_speed_deg_per_day\":%.15f,\"reported_latitude_speed_deg_per_day\":%.15f,\"longitude_speed_deg_per_day\":%.15f,\"latitude_speed_deg_per_day\":%.15f,\"reported_radial_velocity_au_per_day\":%.15f,\"range_derivative_au_per_day\":%.15f}",
      body ? "," : "", body, center_flags, before_flags, after_flags, equatorial_flags,
      state[0], state[1], equatorial[0], equatorial[1], state[2], state[3], state[4], (fmod(after[0]-before[0]+540.0,360.0)-180.0)*1440.0, (after[1]-before[1])*1440.0, state[5], (after[2]-before[2])*1440.0);
  }
  printf("]}\n");
  swe_close();
  return 0;
}
