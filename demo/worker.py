#!/usr/bin/env python3
import concurrent.futures
import functools
import logging
import os
import sys
import time
import urllib.request


log = logging.getLogger('demo')
log.setLevel(logging.DEBUG)
handler = logging.StreamHandler()
handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(message)s"))
log.addHandler(handler)


def fetch_url(url, timeout):
    log.info('Fetching %s', url)
    try:
        with urllib.request.urlopen(url, timeout=timeout):
            return 'ok'
    except Exception as e:
        return str(e)


def main():
    try:
        urls = os.environ['DEMO_URLS'].split(',')
    except KeyError:
        raise Exception('DEMO_URLS should be set') from None

    timeout = float(os.environ.get('DEMO_TIMEOUT', 3.0))
    update_interval = float(os.environ.get('DEMO_UPDATE_INTERVAL', 2.0))

    while True:
        results = {}
        with concurrent.futures.ThreadPoolExecutor(max_workers=len(urls)) as pool:
            for url, result in zip(urls, pool.map(functools.partial(fetch_url, timeout=timeout), urls)):
                results[url] = result
        for url, result in results.items():
            log.info('%s: %s', url, result)
        time.sleep(update_interval)


if __name__ == '__main__':
    try:
        main()
    except KeyboardInterrupt:
        log.warning('Interrupted by user.')
        sys.exit(1)
