import {
  Injectable,
  ConflictException,
  NotFoundException,
  UnauthorizedException,
} from '@nestjs/common';
import { InjectRepository } from '@nestjs/typeorm';
import { Repository, Between, FindOptionsWhere, DataSource } from 'typeorm';
import { randomBytes, createHash } from 'crypto';
import { WaitlistEntry } from './entities/waitlist.entity';
import { JoinWaitlistDto } from './dto/join-waitlist.dto';

const REFERRAL_CODE_MAX_ATTEMPTS = 3;

@Injectable()
export class WaitlistService {
  constructor(
    @InjectRepository(WaitlistEntry)
    private readonly waitlistRepo: Repository<WaitlistEntry>,
    private readonly dataSource: DataSource,
  ) {}

  async join(dto: JoinWaitlistDto) {
    const existing = await this.waitlistRepo.findOne({ where: { email: dto.email } });
    if (existing) throw new ConflictException('Email already on waitlist');

    const rawToken = randomBytes(32).toString('hex');
    const unsubscribeTokenHash = createHash('sha256').update(rawToken).digest('hex');
    const referralCode = await this.generateUniqueReferralCode();

    // Wrap referral crediting and entry creation in a single atomic transaction
    await this.dataSource.transaction(async (manager) => {
      // Credit referrer if valid code provided
      if (dto.referralCode) {
        const referrer = await manager.findOne(WaitlistEntry, {
          where: { referralCode: dto.referralCode },
        });
        if (referrer && referrer.email !== dto.email) {
          await manager.increment(WaitlistEntry, { id: referrer.id }, 'referrals', 1);
        }
      }

      const entry = manager.create(WaitlistEntry, {
        ...dto,
        unsubscribeTokenHash,
        referralCode,
      });
      await manager.save(WaitlistEntry, entry);
    });

    // Return the raw token so it can be included in the confirmation email
    return { rawToken };
  }

  /**
   * Generate a referral code that does not already exist in the database.
   * Retries up to REFERRAL_CODE_MAX_ATTEMPTS times on collision before failing.
   */
  private async generateUniqueReferralCode(): Promise<string> {
    for (let attempt = 0; attempt < REFERRAL_CODE_MAX_ATTEMPTS; attempt++) {
      const candidate = randomBytes(6).toString('hex').toUpperCase();
      const collision = await this.waitlistRepo.findOne({
        where: { referralCode: candidate },
      });
      if (!collision) return candidate;
    }
    throw new ConflictException(
      'Unable to generate a unique referral code, please try again',
    );
  }

  async checkUsername(username: string): Promise<{ available: boolean }> {
    const existing = await this.waitlistRepo.findOne({
      where: { username: username.toLowerCase() },
    });
    return { available: !existing };
  }

  async getStats() {
    const total = await this.waitlistRepo.count();
    return { total };
  }

  async getPosition(email: string): Promise<{ position: number }> {
    const entry = await this.waitlistRepo.findOne({ where: { email } });
    if (!entry) throw new NotFoundException('Email not found on waitlist');
    const position = await this.waitlistRepo.count({
      where: { createdAt: Between(new Date(0), entry.createdAt) },
    });
    return { position };
  }

  /** One-click unsubscribe — no login required, token from email link */
  async unsubscribe(token: string): Promise<{ message: string }> {
    const tokenHash = createHash('sha256').update(token).digest('hex');
    const entry = await this.waitlistRepo.findOne({ where: { unsubscribeTokenHash: tokenHash } });
    if (!entry) throw new UnauthorizedException('Invalid or expired unsubscribe token');
    await this.waitlistRepo.update(entry.id, { unsubscribed: true });
    return { message: 'Successfully unsubscribed' };
  }

  /** Full GDPR deletion — removes all PII, requires email + token */
  async deleteEntry(email: string, token: string): Promise<{ message: string }> {
    const tokenHash = createHash('sha256').update(token).digest('hex');
    const entry = await this.waitlistRepo.findOne({
      where: { email, unsubscribeTokenHash: tokenHash },
    });
    if (!entry) throw new UnauthorizedException('Invalid token or email not found');
    await this.waitlistRepo.delete(entry.id);
    // In production: queue a confirmation email here
    return { message: 'Your data has been deleted. A confirmation email will be sent shortly.' };
  }

  /** Export waitlist as CSV — admin use */
  async exportCsv(opts: {
    country?: string;
    dateFrom?: string;
    dateTo?: string;
    includeUnsubscribed?: boolean;
  }): Promise<string> {
    const where: FindOptionsWhere<WaitlistEntry> = {};
    if (!opts.includeUnsubscribed) where.unsubscribed = false;
    if (opts.country) where.country = opts.country;

    let entries = await this.waitlistRepo.find({ where, order: { createdAt: 'ASC' } });

    if (opts.dateFrom || opts.dateTo) {
      const from = opts.dateFrom ? new Date(opts.dateFrom) : new Date(0);
      const to = opts.dateTo ? new Date(opts.dateTo) : new Date();
      entries = entries.filter((e) => e.createdAt >= from && e.createdAt <= to);
    }

    const header = 'email,username,businessName,country,position,referrals,createdAt';
    const rows = entries.map((e, i) =>
      [
        e.email,
        e.username ?? '',
        e.businessName ?? '',
        e.country ?? '',
        i + 1,
        e.referrals,
        e.createdAt.toISOString(),
      ]
        .map((v) => sanitizeCsvField(v))
        .join(','),
    );

    return [header, ...rows].join('\n');
  }
}

/**
 * Sanitize a CSV field value to prevent formula injection (CWE-1236).
 * Fields starting with =, +, -, @, \t, or \r are prefixed with a single quote
 * to neutralize spreadsheet formula execution in Excel / LibreOffice.
 */
function sanitizeCsvField(value: string | number): string {
  const str = String(value);
  const formulaTriggers = ['=', '+', '-', '@', '\t', '\r'];
  const needsSanitizing = formulaTriggers.some((trigger) => str.startsWith(trigger));
  const sanitized = needsSanitizing ? `'${str}` : str;
  return `"${sanitized.replace(/"/g, '""')}"`;
}
