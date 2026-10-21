import { Test, TestingModule } from '@nestjs/testing';
import { getRepositoryToken } from '@nestjs/typeorm';
import {
  ConflictException,
  NotFoundException,
  UnauthorizedException,
} from '@nestjs/common';
import { Repository } from 'typeorm';
import { WaitlistService } from './waitlist.service';
import { WaitlistEntry } from './entities/waitlist.entity';

// ── Helpers ────────────────────────────────────────────────────────────────

function makeEntry(overrides: Partial<WaitlistEntry> = {}): WaitlistEntry {
  return Object.assign(new WaitlistEntry(), {
    id: 'entry-1',
    email: 'founder@example.com',
    username: 'founder',
    businessName: 'Acme',
    country: 'NG',
    referrals: 0,
    referralCode: 'ABC123',
    unsubscribed: false,
    unsubscribeToken: 'token-abc',
    notified: false,
    createdAt: new Date('2024-01-15T00:00:00Z'),
    ...overrides,
  });
}

function makeRepo(): jest.Mocked<
  Pick<
    Repository<WaitlistEntry>,
    'findOne' | 'create' | 'save' | 'count' | 'find' | 'update' | 'delete' | 'increment'
  >
> {
  return {
    findOne: jest.fn(),
    create: jest.fn(),
    save: jest.fn(),
    count: jest.fn(),
    find: jest.fn(),
    update: jest.fn(),
    delete: jest.fn(),
    increment: jest.fn(),
  };
}

// ── Tests ──────────────────────────────────────────────────────────────────

describe('WaitlistService', () => {
  let service: WaitlistService;
  let repo: ReturnType<typeof makeRepo>;

  beforeEach(async () => {
    repo = makeRepo();

    const module: TestingModule = await Test.createTestingModule({
      providers: [
        WaitlistService,
        { provide: getRepositoryToken(WaitlistEntry), useValue: repo },
      ],
    }).compile();

    service = module.get<WaitlistService>(WaitlistService);
  });

  // ── join ──────────────────────────────────────────────────────────────

  describe('join', () => {
    const dto = {
      email: 'new@example.com',
      username: 'newuser',
      businessName: 'NewCo',
      country: 'US',
    };

    it('creates and saves a new waitlist entry', async () => {
      repo.findOne.mockResolvedValue(null);
      const entry = makeEntry({ email: dto.email });
      repo.create.mockReturnValue(entry);
      repo.save.mockResolvedValue(entry);

      const result = await service.join(dto);

      expect(repo.create).toHaveBeenCalled();
      expect(repo.save).toHaveBeenCalled();
      expect(result).toBe(entry);
    });

    it('generates a unique unsubscribeToken and referralCode', async () => {
      repo.findOne.mockResolvedValue(null);
      repo.create.mockImplementation((data) => data as WaitlistEntry);
      repo.save.mockImplementation(async (e) => e as WaitlistEntry);

      await service.join(dto);

      const created = repo.create.mock.calls[0][0] as Partial<WaitlistEntry>;
      expect(created.unsubscribeToken).toBeDefined();
      expect(created.referralCode).toBeDefined();
      expect(created.unsubscribeToken!.length).toBeGreaterThan(0);
      expect(created.referralCode!.length).toBeGreaterThan(0);
    });

    it('retries referral code generation when a collision is detected', async () => {
      // First findOne (email duplicate check) → null.
      // Then referral-code existence checks: first collides, second is free.
      repo.findOne
        .mockResolvedValueOnce(null)
        .mockResolvedValueOnce(makeEntry({ referralCode: 'COLLIDE' }))
        .mockResolvedValueOnce(null);
      repo.create.mockImplementation((data) => data as WaitlistEntry);
      repo.save.mockImplementation(async (e) => e as WaitlistEntry);

      await service.join(dto);

      // One email check + two referral-code existence checks (retry exercised).
      expect(repo.findOne).toHaveBeenCalledTimes(3);
      expect(repo.save).toHaveBeenCalled();
    });

    it('throws ConflictException when email is already on the waitlist', async () => {
      repo.findOne.mockResolvedValue(makeEntry());

      await expect(service.join(dto)).rejects.toThrow(ConflictException);
    });

    it('credits the referrer when a valid referral code is provided', async () => {
      const referrer = makeEntry({ id: 'referrer-1', referralCode: 'REF001' });
      // First findOne for duplicate check → null; second for referral lookup → referrer
      repo.findOne
        .mockResolvedValueOnce(null)
        .mockResolvedValueOnce(referrer);
      repo.create.mockImplementation((data) => data as WaitlistEntry);
      repo.save.mockImplementation(async (e) => e as WaitlistEntry);
      repo.increment.mockResolvedValue(undefined as any);

      await service.join({ ...dto, referralCode: 'REF001' });

      expect(repo.increment).toHaveBeenCalledWith({ id: 'referrer-1' }, 'referrals', 1);
    });

    it('does not credit the referrer when the referral code is the joiner\'s own', async () => {
      const same = makeEntry({ email: dto.email, referralCode: 'SELF' });
      repo.findOne
        .mockResolvedValueOnce(null)
        .mockResolvedValueOnce(same);
      repo.create.mockImplementation((data) => data as WaitlistEntry);
      repo.save.mockImplementation(async (e) => e as WaitlistEntry);

      await service.join({ ...dto, referralCode: 'SELF' });

      expect(repo.increment).not.toHaveBeenCalled();
    });
  });

  // ── checkUsername ─────────────────────────────────────────────────────

  describe('checkUsername', () => {
    it('returns { available: true } when username is not taken', async () => {
      repo.findOne.mockResolvedValue(null);
      expect(await service.checkUsername('free_name')).toEqual({ available: true });
    });

    it('returns { available: false } when username is taken', async () => {
      repo.findOne.mockResolvedValue(makeEntry());
      expect(await service.checkUsername('taken')).toEqual({ available: false });
    });
  });

  // ── getStats ──────────────────────────────────────────────────────────

  describe('getStats', () => {
    it('returns the total count of waitlist entries', async () => {
      repo.count.mockResolvedValue(42);
      expect(await service.getStats()).toEqual({ total: 42 });
    });
  });

  // ── getPosition ───────────────────────────────────────────────────────

  describe('getPosition', () => {
    it('returns the position for a known email', async () => {
      const entry = makeEntry();
      repo.findOne.mockResolvedValue(entry);
      repo.count.mockResolvedValue(7);

      expect(await service.getPosition(entry.email)).toEqual({ position: 7 });
    });

    it('throws NotFoundException for an email not on the waitlist', async () => {
      repo.findOne.mockResolvedValue(null);

      await expect(service.getPosition('nobody@example.com')).rejects.toThrow(NotFoundException);
    });
  });

  // ── unsubscribe ───────────────────────────────────────────────────────

  describe('unsubscribe', () => {
    it('marks the entry as unsubscribed when token is valid', async () => {
      const entry = makeEntry();
      repo.findOne.mockResolvedValue(entry);
      repo.update.mockResolvedValue(undefined as any);

      const result = await service.unsubscribe('token-abc');

      expect(repo.update).toHaveBeenCalledWith('entry-1', { unsubscribed: true });
      expect(result.message).toMatch(/unsubscribed/i);
    });

    it('throws UnauthorizedException for an invalid token', async () => {
      repo.findOne.mockResolvedValue(null);

      await expect(service.unsubscribe('bad-token')).rejects.toThrow(UnauthorizedException);
    });
  });

  // ── deleteEntry (GDPR) ────────────────────────────────────────────────

  describe('deleteEntry', () => {
    it('deletes the entry when email and token are valid', async () => {
      const entry = makeEntry();
      repo.findOne.mockResolvedValue(entry);
      repo.delete.mockResolvedValue(undefined as any);

      const result = await service.deleteEntry(entry.email, 'token-abc');

      expect(repo.delete).toHaveBeenCalledWith('entry-1');
      expect(result.message).toBeDefined();
    });

    it('throws UnauthorizedException when email/token pair is invalid', async () => {
      repo.findOne.mockResolvedValue(null);

      await expect(service.deleteEntry('ghost@example.com', 'bad')).rejects.toThrow(
        UnauthorizedException,
      );
    });
  });

  // ── exportCsv ─────────────────────────────────────────────────────────

  describe('exportCsv', () => {
    it('returns CSV with a header row and one data row per entry', async () => {
      const entry = makeEntry();
      repo.find.mockResolvedValue([entry]);

      const csv = await service.exportCsv({});

      const lines = csv.split('\n');
      expect(lines[0]).toContain('email');
      expect(lines[1]).toContain(entry.email);
    });

    it('excludes unsubscribed entries when requested', async () => {
      repo.find.mockResolvedValue([]);

      const csv = await service.exportCsv({ excludeUnsubscribed: true });

      expect(repo.find).toHaveBeenCalledWith(
        expect.objectContaining({ where: { unsubscribed: false } }),
      );
      expect(csv.split('\n').length).toBe(1);
    });
  });
});
