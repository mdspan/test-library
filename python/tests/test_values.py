"""Reading and writing the numbers a user types."""

import unittest

from libtimewarp import values


class Durations(unittest.TestCase):
    def test_one_number_and_one_unit(self):
        self.assertEqual(values.parse_duration("500ms"), 500_000_000)
        self.assertEqual(values.parse_duration("90s"), 90 * 10**9)
        self.assertEqual(values.parse_duration("1.5h"), 5400 * 10**9)
        self.assertEqual(values.parse_duration("365d"), 365 * 86400 * 10**9)
        self.assertEqual(values.parse_duration("1ns"), 1)
        self.assertEqual(values.parse_duration("1us"), 1_000)

    def test_a_bare_number_means_seconds(self):
        self.assertEqual(values.parse_duration("-7200"), values.parse_duration("-2h"))

    def test_a_duration_may_be_negative(self):
        self.assertEqual(values.parse_duration("-2h"), -7200 * 10**9)
        self.assertEqual(values.parse_duration("+2h"), 7200 * 10**9)

    def test_anything_else_is_refused(self):
        for text in ["", "2 h", "2 hours", "h", "2y", "yesterday", "1e9"]:
            with self.assertRaises(ValueError):
                values.parse_duration(text)

    def test_a_duration_reads_back_in(self):
        for nanos in [0, 1, -1, 5400 * 10**9, -7200 * 10**9]:
            self.assertEqual(values.parse_duration(values.format_duration(nanos)), nanos)


class Instants(unittest.TestCase):
    def test_rfc_3339(self):
        self.assertEqual(values.parse_instant("2020-01-01T00:00:00Z"), 1577836800 * 10**9)
        self.assertEqual(values.parse_instant("2020-01-01T00:00:00+00:00"), 1577836800 * 10**9)
        # A timestamp with no zone is read as UTC, which is what the tool prints.
        self.assertEqual(values.parse_instant("2020-01-01T00:00:00"), 1577836800 * 10**9)

    def test_at_and_unix_seconds(self):
        self.assertEqual(values.parse_instant("@1577836800"), 1577836800 * 10**9)
        self.assertEqual(values.parse_instant("@1577836800.5"), 1577836800_500_000_000)

    def test_a_duration_serves_a_monotonic_family(self):
        self.assertEqual(values.parse_instant("3600s"), 3600 * 10**9)

    def test_an_instant_reads_back_in(self):
        nanos = 1577836800_123456789
        printed = values.format_instant(nanos)
        self.assertEqual(printed, "2020-01-01T00:00:00.123456789Z")
        # A timestamp carries microseconds, so the round trip keeps everything above them.
        self.assertEqual(values.parse_instant(printed) // 1000, nanos // 1000)

    def test_anything_else_is_refused(self):
        for text in ["yesterday", "2020-13-01T00:00:00Z", ""]:
            with self.assertRaises(ValueError):
                values.parse_instant(text)


class Rates(unittest.TestCase):
    def test_a_rate_is_at_or_above_zero(self):
        self.assertEqual(values.parse_rate("10"), 10.0)
        self.assertEqual(values.parse_rate("0"), 0.0)
        self.assertEqual(values.parse_rate("0.5"), 0.5)
        for text in ["-1", "nan", "fast"]:
            with self.assertRaises(ValueError):
                values.parse_rate(text)


class ClockNames(unittest.TestCase):
    def test_the_platform_names_its_own_clocks(self):
        import time

        self.assertEqual(values.clock_name(time.CLOCK_REALTIME), "CLOCK_REALTIME")
        self.assertEqual(values.clock_name(time.CLOCK_MONOTONIC), "CLOCK_MONOTONIC")
        # Cpu tracks no timebase, and an id this platform has no name for is the number itself.
        self.assertEqual(values.clock_name(None), "")
        self.assertEqual(values.clock_name(0x7FFFFFFF), str(0x7FFFFFFF))

    def test_an_id_with_two_names_goes_by_the_one_that_is_not_an_alias(self):
        import time

        # FreeBSD spells `CLOCK_BOOTTIME` as `CLOCK_UPTIME` through 14.x and as `CLOCK_MONOTONIC`
        # from 15.0. Elsewhere it is a clock of its own, or absent, and this checks nothing.
        boottime = getattr(time, "CLOCK_BOOTTIME", None)
        for name in ["CLOCK_UPTIME", "CLOCK_MONOTONIC"]:
            if boottime is not None and getattr(time, name, None) == boottime:
                self.assertEqual(values.clock_name(boottime), name)


if __name__ == "__main__":
    unittest.main()
