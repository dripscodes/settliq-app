import { plainToInstance } from 'class-transformer';
import { validate } from 'class-validator';
import { RegisterDto } from './register.dto';

describe('RegisterDto', () => {
  const buildDto = (password: string): RegisterDto =>
    plainToInstance(RegisterDto, { email: 'user@example.com', password });

  const passwordErrors = async (password: string): Promise<string[]> => {
    const errors = await validate(buildDto(password));
    const passwordError = errors.find((error) => error.property === 'password');
    return passwordError ? Object.keys(passwordError.constraints ?? {}) : [];
  };

  it('accepts a valid secure password', async () => {
    const errors = await validate(buildDto('SecurePass123!'));
    expect(errors).toHaveLength(0);
  });

  it('rejects passwords shorter than 8 characters', async () => {
    const constraints = await passwordErrors('Ab1!');
    expect(constraints).toContain('minLength');
  });

  it('rejects passwords longer than 72 characters', async () => {
    const longPassword = `Aa1!${'a'.repeat(69)}`;
    expect(longPassword).toHaveLength(73);
    const constraints = await passwordErrors(longPassword);
    expect(constraints).toContain('maxLength');
  });

  it('accepts a password of exactly 72 characters', async () => {
    const maxPassword = `Aa1!${'a'.repeat(68)}`;
    expect(maxPassword).toHaveLength(72);
    const errors = await validate(buildDto(maxPassword));
    expect(errors).toHaveLength(0);
  });

  it('rejects passwords without an uppercase letter', async () => {
    const constraints = await passwordErrors('securepass123!');
    expect(constraints).toContain('matches');
  });

  it('rejects passwords without a lowercase letter', async () => {
    const constraints = await passwordErrors('SECUREPASS123!');
    expect(constraints).toContain('matches');
  });

  it('rejects passwords without a digit or special character', async () => {
    const constraints = await passwordErrors('SecurePassword');
    expect(constraints).toContain('matches');
  });
});
