"""Development API OAuth and admin smoke cases."""
import re
import secrets
import urllib.error
from dev_api_totp import totp


class AccountFlows:
    def oauth_flow(self, a, b):
        form = urllib.parse.urlencode({'client_id': 'respire-cli', 'device_name': 'API acceptance', 'expected_user': a['user']})
        grant = self.check('POST', '/oauth/device/code', form,
                           predicate=lambda r: bool(re.fullmatch('[0-9a-fA-F]{12}', r.get('user_code', ''))) and
                           urllib.parse.urlsplit(r.get('verification_uri_complete', '')).netloc == 'dash.dev.rsrs.rs',
                           label='device-grant-development-dashboard')
        path = '/api/self/cli-authorization/' + grant['user_code']
        self.check('GET', path, token=a['token'], predicate=lambda r: r.get('device_name') == 'API acceptance',
                   label='device-grant-details')
        self.check('POST', path, {'approve': True}, b['token'], status=409,
                   predicate=lambda r: 'error' in r, kind='negative', label='wrong-account-approval-rejected')
        self.check('POST', path, {'approve': True}, a['token'], predicate=lambda r: r.get('ok') is True,
                   label='device-grant-approved')
        poll = urllib.parse.urlencode({'client_id': 'respire-cli', 'grant_type': 'urn:ietf:params:oauth:grant-type:device_code',
                                      'device_code': grant['device_code']})
        access = self.check('POST', '/oauth/token', poll,
                            predicate=lambda r: r.get('token_type') == 'Bearer' and r.get('user') == a['user'] and bool(r.get('access_token')),
                            label='approved-grant-exchanged')
        self.check('GET', '/api/self', token=access['access_token'], predicate=lambda r: r.get('user') == a['user'],
                   label='oauth-session-account-readback')
        self.check('POST', '/oauth/token', poll, status=400, predicate=lambda r: r.get('error') == 'invalid_grant',
                   kind='negative', label='device-grant-replay-rejected')
        self.check('GET', '/api/self/cli-authorization/000000000000', token=a['token'], status=404,
                   predicate=lambda r: 'error' in r, kind='negative', label='unknown-authorization-code')

    def admin_flow(self, owner, a):
        self.owned(owner['user'], admin=True)
        token = owner['token']
        self.check('GET', '/admin/me', token=token, predicate=lambda r: r.get('user') == owner['user'] and r.get('role') == 'owner', label='fixture-owner-identity')
        viewer = self.create_admin('viewer', 'viewer', token)
        admin = self.create_admin('operator', 'admin', token)
        def valid_stats(reply, days):
            since = reply.get('memory_tracking_since', '')
            points = reply.get('series', [])
            return (reply.get('days') == days and reply.get('timezone') == 'Asia/Shanghai'
                    and reply.get('historical_baseline') == 'retained_registrations_and_sessions'
                    and bool(re.fullmatch(r'\d{4}-\d{2}-\d{2}', since)) and len(points) == days
                    and all((i == 0 or points[i - 1]['date'] < point['date'])
                            and type(point.get('registrations')) is int and point['registrations'] >= 0
                            and type(point.get('sessions')) is int and point['sessions'] >= 0
                            and (point.get('memories') is None if point['date'] < since else
                                 type(point.get('memories')) is int and point['memories'] >= 0)
                            for i, point in enumerate(points)))
        self.check('GET', '/admin/stats', token=token, predicate=lambda r: valid_stats(r, 30), label='owner-daily-stats-default')
        for days in [7, 90]:
            self.check('GET', f'/admin/stats?days={days}', token=admin['token'],
                       predicate=lambda r, days=days: valid_stats(r, days), label=f'admin-daily-stats-{days}-days')
        for days in ['6', '91', 'abc', '7.5']:
            self.check('GET', '/admin/stats?days=' + days, token=token, status=400,
                       predicate=lambda r: 'error' in r, kind='negative', label='stats-invalid-days-rejected')
        self.check('GET', '/admin/stats', token=viewer['token'], status=403,
                   predicate=lambda r: r.get('error') == 'forbidden', kind='negative', label='viewer-daily-stats-blocked')
        self.check('GET', '/admin/admins', token=token,
                   predicate=lambda r: any(x.get('user') == viewer['user'] for x in r.get('admins', [])), label='fixture-admin-visible')
        forbidden = self.namespace + '-forbidden'
        self.unconfirmed_creations.add(forbidden)
        self.check('POST', '/admin/admins', {'user': forbidden, 'pass_hash': 'aa', 'role': 'viewer'}, admin['token'],
                   status=403, predicate=lambda r: 'error' in r, kind='negative', label='non-owner-admin-management-blocked')
        self.unconfirmed_creations.remove(forbidden)
        self.unconfirmed_creations.add(forbidden)
        self.check('POST', '/admin/users', {'user': forbidden, 'pass_hash': 'aa'}, viewer['token'],
                   status=403, predicate=lambda r: 'error' in r, kind='negative', label='viewer-write-blocked')
        self.unconfirmed_creations.remove(forbidden)
        self.check('GET', '/admin/admins', token=viewer['token'], status=403,
                   predicate=lambda r: 'error' in r, kind='negative', label='viewer-admin-list-blocked')
        self.check('GET', '/admin/me', token=a['token'], status=403,
                   predicate=lambda r: 'error' in r, kind='negative', label='end-user-admin-access-blocked')
        self.owned(viewer['user'], admin=True)
        self.check('POST', '/admin/admins/' + viewer['user'] + '/update', {'email': viewer['user'] + '@example.invalid'}, token,
                   predicate=lambda r: r.get('updated') is True, label='fixture-admin-update')
        self.check('GET', '/admin/admins', token=token,
                   predicate=lambda r: any(x.get('user') == viewer['user'] and x.get('email') == viewer['user'] + '@example.invalid'
                                           for x in r.get('admins', [])), label='fixture-admin-update-readback')
        owner['pass_hash'] = secrets.token_hex(32)
        self.check('POST', '/admin/password', {'pass_hash': owner['pass_hash'], 'salt': owner['salt']}, token,
                   predicate=lambda r: r.get('updated') is True, label='fixture-owner-password-change')
        relogin = self.check('POST', '/admin/login', {'user': owner['user'], 'pass_hash': owner['pass_hash']},
                             predicate=lambda r: bool(r.get('token')), label='fixture-owner-new-password-login')
        owner['token'] = token = relogin['token']
        setup = self.check('POST', '/admin/totp/begin', {}, token, predicate=lambda r: bool(r.get('secret')), label='fixture-admin-totp-setup')
        self.check('POST', '/admin/totp/confirm', {'code': totp(setup['secret'])}, token, predicate=lambda r: r.get('totp') is True, label='fixture-admin-totp-enable')
        challenge = self.check('POST', '/admin/login', {'user': owner['user'], 'pass_hash': owner['pass_hash']},
                               predicate=lambda r: r.get('totp_required') is True and bool(r.get('ticket')), label='fixture-admin-totp-challenge')
        login = self.check('POST', '/admin/login/totp', {'ticket': challenge['ticket'], 'code': totp(setup['secret'])},
                           predicate=lambda r: bool(r.get('token')), label='fixture-admin-totp-login')
        owner['token'] = token = login['token']
        self.check('POST', '/admin/totp/disable', {'code': totp(setup['secret'])}, token,
                   predicate=lambda r: r.get('totp') is False, label='fixture-admin-totp-disable')
        target = self.namespace + '-managed'
        password = secrets.token_hex(32)
        salt = secrets.token_hex(16)
        self.unconfirmed_creations.add(target)
        created = self.check('POST', '/admin/users', {'user': target, 'pass_hash': password, 'salt': salt}, token,
                             predicate=lambda r: r.get('user') == target and bool(r.get('token')), label='fixture-managed-user-create')
        account = {'user': target, 'pass_hash': password, 'salt': salt, 'token': created['token']}
        self.users[target] = account
        self.unconfirmed_creations.remove(target)
        self.check('GET', '/admin/users?q=' + target + '&page=1&limit=10', token=token,
                   predicate=lambda r: r.get('total') == 1 and r.get('users', [{}])[0].get('user') == target, label='scoped-user-search')
        self.check('GET', '/admin/users?q=' + target + '&export=1', token=token,
                   predicate=lambda r: isinstance(r.get('csv'), str) and target in r['csv'], label='scoped-user-csv')
        self.owned(target)
        self.check('POST', '/admin/users/' + target + '/update', {'email': target + '@example.invalid'}, token,
                   predicate=lambda r: r.get('updated') is True, label='fixture-managed-user-update')
        self.check('GET', '/admin/users?q=' + target + '&page=1&limit=10', token=token,
                   predicate=lambda r: any(x.get('user') == target and x.get('email') == target + '@example.invalid'
                                           for x in r.get('users', [])), label='fixture-managed-user-update-readback')
        session = self.check('POST', '/admin/users/' + target + '/sessions', {'device_name': 'managed-api'}, token,
                             predicate=lambda r: bool(r.get('token')) and bool(r.get('session_id')), label='fixture-managed-session-create')
        self.check('GET', '/admin/users/' + target + '/sessions', token=token,
                   predicate=lambda r: any(x.get('id') == session['session_id'] for x in r.get('sessions', [])), label='fixture-managed-sessions-list')
        self.check('POST', '/admin/users/' + a['user'] + '/sessions/' + session['session_id'] + '/revoke', {}, token,
                   status=404, predicate=lambda r: 'error' in r, kind='negative', label='mismatched-session-user-rejected')
        self.check('POST', '/admin/users/' + target + '/sessions/' + session['session_id'] + '/revoke', {}, token,
                   predicate=lambda r: r.get('revoked') is True, label='fixture-managed-session-revoke')
        self.check('GET', '/count', token=session['token'], status=401, predicate=lambda r: 'error' in r, kind='negative', label='managed-revoked-token-rejected')
        self.check('POST', '/push', self.blob(self.namespace + '-managed-data'), account['token'], predicate=lambda r: r.get('replaced') is True, label='managed-data-before-account-transitions')
        for action, field in [('disable', 'disabled'), ('enable', 'disabled'), ('kick', 'kicked'), ('delete', 'deleted'), ('restore', 'restored')]:
            self.owned(target)
            before = account['token']
            self.check('POST', '/admin/users/' + target + '/' + action, {}, token,
                       predicate=lambda r, f=field, act=action: r.get(f) is (act != 'enable'), label='fixture-account-' + action)
            if action == 'kick':
                self.check('GET', '/count', token=before, status=401, predicate=lambda r: 'error' in r,
                           kind='negative', label='kicked-account-old-token-rejected')
            if action in ('disable', 'delete'):
                self.login(account, 403)
            elif action in ('enable', 'kick', 'restore'):
                self.login(account)
        old = account['token']
        rotated = self.check('POST', '/admin/users/' + target + '/rotate', {}, token,
                             predicate=lambda r: bool(r.get('token')) and r['token'] != old, label='fixture-admin-token-rotate')
        account['token'] = rotated['token']
        self.check('GET', '/pull', token=old, status=401, predicate=lambda r: 'error' in r, kind='negative', label='admin-rotation-old-token-rejected')
        self.check('GET', '/pull', token=account['token'], predicate=lambda r: len(r.get('blobs', [])) == 1, label='managed-data-survives-soft-transitions')
        self.check('GET', '/admin/audit?q=' + self.namespace + '&page=1&limit=100', token=token,
                   predicate=lambda r: r.get('total', 0) > 0 and bool(r.get('items')), label='fixture-audit-present')
        for suffix, alias in [('revoke-target', 'revoke'), ('purge-target', 'purge')]:
            account = self.register(suffix)
            self.owned(account['user'])
            self.check('POST', '/push', self.blob(self.namespace + '-' + suffix + '-blob'), account['token'],
                       predicate=lambda r: r.get('replaced') is True, label='fixture-data-before-admin-purge')
            self.check('POST', '/admin/users/' + account['user'] + '/' + alias, {}, token,
                       predicate=lambda r: r.get('purged') is True and r.get('blobs_deleted', 0) >= 1, label='fixture-admin-' + alias + '-alias')
            del self.users[account['user']]
            self.check('GET', '/count', token=account['token'], status=401, predicate=lambda r: 'error' in r, kind='negative', label='purged-account-token-rejected')
        for fixture in (viewer, admin):
            self.owned(fixture['user'], admin=True)
            self.check('POST', '/admin/admins/' + fixture['user'] + '/revoke', {}, token,
                       predicate=lambda r: r.get('deleted') is True, label='fixture-admin-delete')
            self.admins.remove(fixture['user'])
            self.check('POST', '/admin/login', {'user': fixture['user'], 'pass_hash': fixture['pass_hash']},
                       status=401, predicate=lambda r: 'error' in r, kind='negative', label='deleted-admin-login-rejected')
            self.check('GET', '/admin/me', token=fixture['token'], status=403,
                       predicate=lambda r: 'error' in r, kind='negative', label='deleted-admin-token-rejected')
