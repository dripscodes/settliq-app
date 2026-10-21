import {
  Injectable,
  UnauthorizedException,
  ConflictException,
  NotFoundException,
} from '@nestjs/common';
import { JwtService } from '@nestjs/jwt';
import { InjectRepository } from '@nestjs/typeorm';
import { Repository } from 'typeorm';
import * as bcrypt from 'bcrypt';
import * as crypto from 'crypto';
import { Merchant, MerchantStatus } from '../merchants/entities/merchant.entity';
import { RefreshToken } from './refresh-token.entity';
import { RegisterDto } from './dto/register.dto';
import { LoginDto } from './dto/login.dto';
import { AuthTokenResponseDto } from './dto/auth-token-response.dto';

/** Lifetime of a refresh token: 30 days. */
const REFRESH_TOKEN_TTL_MS = 30 * 24 * 60 * 60 * 1000;

/** Number of random bytes in a generated API key (256 bits of entropy). */
const API_KEY_BYTES = 32;

/** Prefix applied to generated API keys for easy identification. */
const API_KEY_PREFIX = 'sk_live_';

/** Result of generating or rotating an API key. The plaintext key is only returned once. */
export interface GeneratedApiKey {
  apiKey: string;
  createdAt: Date;
}

@Injectable()
export class AuthService {
  constructor(
    @InjectRepository(Merchant)
    private readonly merchantsRepo: Repository<Merchant>,
    @InjectRepository(RefreshToken)
    private readonly refreshTokenRepo: Repository<RefreshToken>,
    private readonly jwtService: JwtService,
  ) {}

  async register(dto: RegisterDto): Promise<AuthTokenResponseDto> {
    const existing = await this.merchantsRepo.findOne({ where: { email: dto.email } });
    if (existing) throw new ConflictException('Email already registered');

    const passwordHash = await bcrypt.hash(dto.password, 12);
    const merchant = this.merchantsRepo.create({
      email: dto.email,
      passwordHash,
      businessName: dto.businessName,
      businessType: dto.businessType,
      country: dto.country,
      status: MerchantStatus.ACTIVE,
    });

    const saved = await this.merchantsRepo.save(merchant);
    return this.issueTokenPair(saved);
  }

  async login(dto: LoginDto): Promise<AuthTokenResponseDto> {
    const merchant = await this.merchantsRepo.findOne({ where: { email: dto.email } });
    if (!merchant) throw new UnauthorizedException('Invalid credentials');

    const valid = await bcrypt.compare(dto.password, merchant.passwordHash);
    if (!valid) throw new UnauthorizedException('Invalid credentials');

    return this.issueTokenPair(merchant);
  }

  /**
   * Rotate a refresh token.
   *
   * Flow:
   *  1. SHA-256 hash the incoming opaque token.
   *  2. Look up the stored record by hash.
   *  3. Reject if not found (already rotated / never issued) — 401.
   *  4. Reject if expired — 401.
   *  5. Delete the old token record (single-use rotation).
   *  6. Issue and return a fresh token pair.
   */
  async refresh(rawToken: string): Promise<AuthTokenResponseDto> {
    const tokenHash = this.hashToken(rawToken);

    const stored = await this.refreshTokenRepo.findOne({
      where: { tokenHash },
      relations: ['merchant'],
    });

    if (!stored) {
      throw new UnauthorizedException('Invalid or already-used refresh token');
    }

    if (stored.expiresAt < new Date()) {
      // Clean up the expired record and reject.
      await this.refreshTokenRepo.delete({ id: stored.id });
      throw new UnauthorizedException('Refresh token expired');
    }

    const merchant = stored.merchant;

    // Invalidate the used token before issuing a new pair (rotation).
    await this.refreshTokenRepo.delete({ id: stored.id });

    return this.issueTokenPair(merchant);
  }

  /**
   * Generate a new API key for the given merchant.
   *
   * The plaintext key is returned exactly once; only the bcrypt hash and the
   * SHA-256 lookup hash are persisted. Any previously active key is replaced.
   */
  async generateApiKey(merchantId: string): Promise<GeneratedApiKey> {
    const merchant = await this.merchantsRepo.findOne({ where: { id: merchantId } });
    if (!merchant) throw new NotFoundException('Merchant not found');

    const rawKey = this.createRawApiKey();
    const { apiKeyHash, apiKeyLookupHash } = await this.hashApiKey(rawKey);

    merchant.apiKeyHash = apiKeyHash;
    merchant.apiKeyLookupHash = apiKeyLookupHash;
    await this.merchantsRepo.save(merchant);

    return { apiKey: rawKey, createdAt: new Date() };
  }

  /**
   * Rotate the merchant's API key. The previous key is invalidated immediately
   * (its hash is overwritten) and a fresh plaintext key is returned once.
   */
  async rotateApiKey(merchantId: string): Promise<GeneratedApiKey> {
    return this.generateApiKey(merchantId);
  }

  /**
   * Revoke the merchant's active API key by clearing the stored hashes.
   * Idempotent: revoking when no key exists is a no-op.
   */
  async revokeApiKey(merchantId: string): Promise<void> {
    const merchant = await this.merchantsRepo.findOne({ where: { id: merchantId } });
    if (!merchant) throw new NotFoundException('Merchant not found');

    merchant.apiKeyHash = null;
    merchant.apiKeyLookupHash = null;
    await this.merchantsRepo.save(merchant);
  }

  /**
   * Looks up a merchant by API key in O(1) via an indexed SHA-256 lookup hash,
   * then verifies the bcrypt hash on only that one candidate record.
   *
   * The previous implementation scanned every merchant with an API key and ran
   * bcrypt.compare sequentially (~250ms × N), making it trivially exploitable
   * as a DoS vector: a single unauthenticated request with any x-api-key header
   * would block the event loop for seconds (100 merchants → ~25 s).
   *
   * Requires the `apiKeyLookupHash` column (SHA-256 of the raw key) to be
   * populated atomically alongside `apiKeyHash` when a key is issued or rotated.
   */
  async findMerchantByApiKey(rawKey: string): Promise<Merchant | null> {
    const lookupHash = crypto.createHash('sha256').update(rawKey).digest('hex');

    const merchant = await this.merchantsRepo.findOne({
      where: { apiKeyLookupHash: lookupHash },
      withDeleted: false,
    });

    if (!merchant?.apiKeyHash) {
      return null;
    }

    const valid = await bcrypt.compare(rawKey, merchant.apiKeyHash);
    if (!valid) {
      return null;
    }

    if (merchant.status !== MerchantStatus.ACTIVE || merchant.deletedAt) {
      throw new UnauthorizedException('Merchant account is suspended or inactive');
    }

    return merchant;
  }

  // ---------------------------------------------------------------------------
  // Private helpers
  // ---------------------------------------------------------------------------

  /** Generate a fresh access + refresh token pair, persist the hashed refresh
   * token, and return the full response DTO.
   */
  private async issueTokenPair(merchant: Merchant): Promise<AuthTokenResponseDto> {
    const accessToken = this.signToken(merchant.id, merchant.email, merchant.role);

    // Generate a cryptographically random opaque refresh token.
    const rawRefreshToken = crypto.randomBytes(40).toString('hex');
    const tokenHash = this.hashToken(rawRefreshToken);

    const record = this.refreshTokenRepo.create({
      tokenHash,
      merchantId: merchant.id,
      merchant,
      expiresAt: new Date(Date.now() + REFRESH_TOKEN_TTL_MS),
    });
    await this.refreshTokenRepo.save(record);

    const { passwordHash: _, apiKeyHash: __, ...merchantData } = merchant as any;
    return { accessToken, refreshToken: rawRefreshToken, merchant: merchantData };
  }

  /** Create a new random plaintext API key with the standard prefix. */
  private createRawApiKey(): string {
    return `${API_KEY_PREFIX}${crypto.randomBytes(API_KEY_BYTES).toString('hex')}`;
  }

  /**
   * Derive the persisted hashes for a raw API key:
   *  - `apiKeyHash`: bcrypt hash used for constant-time verification.
   *  - `apiKeyLookupHash`: SHA-256 hex digest used for O(1) indexed lookup.
   */
  private async hashApiKey(
    rawKey: string,
  ): Promise<{ apiKeyHash: string; apiKeyLookupHash: string }> {
    const apiKeyHash = await bcrypt.hash(rawKey, 12);
    const apiKeyLookupHash = crypto.createHash('sha256').update(rawKey).digest('hex');
    return { apiKeyHash, apiKeyLookupHash };
  }

  /** SHA-256 hex digest of a raw token string. */
  private hashToken(raw: string): string {
    return crypto.createHash('sha256').update(raw).digest('hex');
  }

  private signToken(sub: string, email: string, role?: string): string {
    return this.jwtService.sign({ sub, email, role });
  }
}
